use crossbeam_channel::{RecvError, SendError};
use log::Level::*;
use std::collections::HashMap;
use std::time::Duration;

use crate::crossbeam::{self, ChannelVariant};
use crate::detthread::DetThreadId;
use crate::error::DesyncError;
use crate::recordlog::{self, ChannelLabel, RecordedEvent, SelectEvent};
use crate::rr::DetChannelId;
use crate::{desync, detthread};
use crate::{DesyncMode, RRMode};
use crate::{DESYNC_MODE, RECORD_MODE};

/// Our Select wrapper type must hold different types of Receivet<T>. We use this trait
/// so we can hold different T's via a dynamic trait object.
trait BufferedReceiver {
    /// Returns true if there is any buffered values in this receiver.
    fn buffered_value(&self) -> bool;
    /// Returns true if there is an entry in this receiver waiting to be read.
    /// Returns false if no entry arrives by `timeout`.
    fn poll(&self, sender: &Option<DetThreadId>, timeout: Duration) -> bool;
}

impl<T> BufferedReceiver for crossbeam::Receiver<T> {
    fn buffered_value(&self) -> bool {
        let hashmap = self.buffer.borrow();
        for queue in hashmap.values() {
            if !queue.is_empty() {
                return true;
            }
        }
        return false;
    }

    fn poll(&self, sender: &Option<DetThreadId>, timeout: Duration) -> bool {
        self.poll_entry(sender, timeout)
    }
}

/// Wrapper type around crossbeam's Select.
pub struct Select<'a> {
    selector: crossbeam_channel::Select<'a>,
    mode: RRMode,
    // Use existential to abstract over the `T` which may vary per receiver. We only care
    // if it has entries anyways, not the T contained within.
    receivers: HashMap<usize, &'a dyn BufferedReceiver>,
}

impl<'a> Select<'a> {
    /// Creates an empty list of channel operations for selection.
    pub fn new() -> Select<'a> {
        let mode = *RECORD_MODE;
        Select {
            mode,
            selector: crossbeam_channel::Select::new(),
            receivers: HashMap::new(),
        }
    }

    /// Adds a send operation.
    /// Returns the index of the added operation.
    pub fn send<T>(&mut self, s: &'a crossbeam::Sender<T>) -> usize {
        unimplemented!("Send on select not supported?");
        // log_trace("Select::send()");
        // self.selector.send(&s.sender)
    }

    /// Adds a receive operation.
    /// Returns the index of the added operation.
    pub fn recv<T>(&mut self, r: &'a crossbeam::Receiver<T>) -> usize {
        crate::log_rr!(Info, "Select adding receiver<{:?}>", r.metadata.id);

        // We still register all receivers with selector, even on replay. As on
        // desynchonization we will have to select on the real selector.
        let index = match &r.receiver {
            ChannelVariant::After(r) => self.selector.recv(&r),
            ChannelVariant::Bounded(r) | ChannelVariant::Unbounded(r) => self.selector.recv(&r),
            ChannelVariant::Never(r) => self.selector.recv(&r),
        };
        if let Some(v) = self.receivers.insert(index, r) {
            panic!(
                "Entry already exists at {:?} This should be impossible.",
                index
            );
        }
        index
    }

    pub fn select(&mut self) -> SelectedOperation<'a> {
        crate::log_rr!(Info, "Select::select()");
        self.rr_select().unwrap_or_else(|error| {
            crate::log_rr!(Warn, "Select: Desynchronization detected! {:?}", error);

            match *DESYNC_MODE {
                DesyncMode::Panic => {
                    panic!("Select: Desynchronization detected: {:?}", error);
                }
                // Desync for crossbeam select is not so bad. We still register
                // receivers with the selector. So on desync errors we can
                // simply call select() directly, we check if any receiver's buffer
                // has values which we could "flush" first...
                DesyncMode::KeepGoing => {
                    desync::mark_program_as_desynced();
                    for (index, receiver) in self.receivers.iter() {
                        if receiver.buffered_value() {
                            crate::log_rr!(Debug, "Buffered value chosen from receiver.");
                            return SelectedOperation::DesyncBufferEntry(*index);
                        }
                    }
                    // No values in buffers, read directly from selector.
                    crate::log_rr!(Debug, "No value in buffer, calling select directly.");
                    SelectedOperation::Desync(self.selector.select())
                }
            }
        })
    }

    pub fn ready(&mut self) -> usize {
        crate::log_rr!(Info, "Select::ready()");
        self.rr_ready().unwrap_or_else(|error| {
            crate::log_rr!(Warn, "Ready::Desynchronization found: {:?}", error);
            match *DESYNC_MODE {
                DesyncMode::Panic => panic!("Desynchronization detected: {:?}", error),
                DesyncMode::KeepGoing => {
                    desync::mark_program_as_desynced();
                    detthread::inc_event_id();
                    self.selector.ready()
                }
            }
        })
    }

    fn rr_ready(&mut self) -> Result<usize, DesyncError> {
        if desync::program_desyned() {
            return Err(DesyncError::Desynchronized);
        }
        match self.mode {
            RRMode::Record => {
                let select_index = self.selector.ready();
                recordlog::log(
                    RecordedEvent::SelectReady { select_index },
                    ChannelLabel::None,
                    "ready",
                    // Ehh, we fake it here. We never check this value anyways.
                    &DetChannelId::fake(),
                );
                Ok(select_index)
            }
            RRMode::Replay => {
                let det_id = detthread::get_det_id_desync()?;
                let event = recordlog::get_log_entry(det_id, detthread::get_event_id())?;
                detthread::inc_event_id();

                match event {
                    RecordedEvent::SelectReady { select_index } => Ok(*select_index),
                    event => {
                        let dummy = RecordedEvent::SelectReady { select_index: 0 };
                        Err(DesyncError::EventMismatch(event.clone(), dummy))
                    }
                }
            }
            RRMode::NoRR => Ok(self.selector.ready()),
        }
    }

    fn rr_select(&mut self) -> Result<SelectedOperation<'a>, DesyncError> {
        if desync::program_desyned() {
            return Err(DesyncError::Desynchronized);
        }
        match self.mode {
            RRMode::Record => {
                // We don't know the thread_id of sender until the select is complete
                // when user call recv() on SelectedOperation. So do nothing here.
                // Index will be recorded there.
                Ok(SelectedOperation::Record(self.selector.select()))
            }
            RRMode::Replay => {
                let det_id = detthread::get_det_id_desync()?;

                // Query our log to see what index was selected!() during the replay phase.
                // ChannelVariant type not check on Select::select() but on Select::recv()
                let entry = recordlog::get_log_entry_ret(det_id, detthread::get_event_id());

                // Here we put this thread to sleep if the entry is missing.
                // On record, the thread never returned from blocking...
                if let Err(e @ DesyncError::NoEntryInLog(_, _)) = entry {
                    crate::log_rr!(Info, "Saw {:?}. Putting thread to sleep.", e);
                    desync::sleep_until_desync();
                    // Thread woke back up... desynced!
                    return Err(DesyncError::DesynchronizedWakeup);
                }

                let (event, flavor, chan_id) = entry?;
                match event {
                    RecordedEvent::Select(event) => {
                        // Verify the receiver has the entry. We check now, since
                        // once we return the SelectedOperation it is too late if it turns
                        // out the message is not there => "unrecoverable error".
                        if let SelectEvent::Success {
                            selected_index,
                            sender_thread,
                        } = event
                        {
                            crate::log_rr!(
                                Debug,
                                "Polling select receiver to see if message is there..."
                            );
                            match self.receivers.get(selected_index) {
                                Some(receiver) => {
                                    if !receiver.poll(sender_thread, Duration::from_secs(1)) {
                                        crate::log_rr!(Debug, "No such message found via polling.");
                                        // We failed to find the message we were expecting
                                        // this is a desynchonization.
                                        return Err(DesyncError::Timedout);
                                    }
                                    crate::log_rr!(Debug, "Polling verified message is present.");
                                }
                                None => {
                                    crate::log_rr!(Warn, "Missing receiver.");
                                    let i = *selected_index as u64;
                                    return Err(DesyncError::MissingReceiver(i));
                                }
                            }
                        }

                        Ok(SelectedOperation::Replay(event, *flavor, chan_id.clone()))
                    }
                    e => {
                        let dummy = SelectEvent::Success {
                            sender_thread: None,
                            selected_index: (0 - 1) as usize,
                        };
                        Err(DesyncError::EventMismatch(
                            e.clone(),
                            RecordedEvent::Select(dummy),
                        ))
                    }
                }
            }
            RRMode::NoRR => Ok(SelectedOperation::NoRR(self.selector.select())),
        }
    }
}

pub enum SelectedOperation<'a> {
    Replay(&'a SelectEvent, ChannelLabel, DetChannelId),
    /// Desynchonization happened and receiver still has buffered entries. Index
    /// of receiver has been returned to user.
    DesyncBufferEntry(usize),
    Record(crossbeam_channel::SelectedOperation<'a>),
    NoRR(crossbeam_channel::SelectedOperation<'a>),
    /// Desynchonization happened, no entries in buffer, we call selector.select()
    /// to have crossbeam do the work for us.
    Desync(crossbeam_channel::SelectedOperation<'a>),
}

/// A selected operation that needs to be completed.
///
/// To complete the operation, call [`send`] or [`recv`].
///
/// # Panics
///
/// Forgetting to complete the operation is an error and might lead to deadlocks. If a
/// `SelectedOperation` is dropped without completion, a panic occurs.
///
/// [`send`]: struct.SelectedOperation.html#method.send
/// [`recv`]: struct.SelectedOperation.html#method.recv
impl<'a> SelectedOperation<'a> {
    /// Returns the index of the selected operation.
    /// We don't log calls to this method as they will always be determinstic.
    pub fn index(&self) -> usize {
        match self {
            SelectedOperation::DesyncBufferEntry(selected_index)
            | SelectedOperation::Replay(SelectEvent::Success { selected_index, .. }, _, _)
            | SelectedOperation::Replay(SelectEvent::RecvError { selected_index, .. }, _, _) => {
                *selected_index
            }
            SelectedOperation::Record(selected)
            | SelectedOperation::NoRR(selected)
            | SelectedOperation::Desync(selected) => selected.index(),
        }
    }

    /// Completes the send operation.
    ///
    /// The passed [`Sender`] reference must be the same one that was used in [`Select::send`]
    /// when the operation was added.
    ///
    /// # Panics
    ///
    /// Panics if an incorrect [`Sender`] reference is passed.
    pub fn send<T>(self, _s: &crossbeam::Sender<T>, _msg: T) -> Result<(), SendError<T>> {
        panic!("Unimplemented send for record and replay channels.")
    }

    /// Completes the receive operation.
    ///
    /// The passed [`Receiver`] reference must be the same one that was used in [`Select::recv`]
    /// when the operation was added.
    ///
    /// # Panics
    ///
    /// Panics if an incorrect [`Receiver`] reference is passed.
    pub fn recv<T>(self, r: &crossbeam::Receiver<T>) -> Result<T, RecvError> {
        crate::log_rr!(Info, "SelectedOperation<{:?}>::recv()", r.metadata().id);

        match self.rr_select_recv(r) {
            Ok(v) => v,
            Err(error) => {
                // This type of desynchronization cannot be recovered from.
                // This is due to how the Select API works for crossbeam channels...
                // By the time we realize we have the "wrong receiver" e.g. flavor
                // of chan_id mismatch, it is too late for us to do the selector.select()
                // event. We cannot do this event preemptively as crossbeam will panic!()
                // if we do select() but do not finish it by doing recv() on the selected
                // operation. This should not really happen. So I think we're okay.
                crate::log_rr!(
                    Error,
                    "SelectedOperation: Unrecoverable desynchonization: {:?}",
                    error
                );
                panic!(
                    "SelectedOperation: Unrecoverable desynchonization: {:?}",
                    error
                );
            }
        }
    }

    pub fn rr_select_recv<T>(
        self,
        r: &crossbeam::Receiver<T>,
    ) -> Result<Result<T, RecvError>, DesyncError> {
        // Do not add check for program_desynced() here! This type of desync
        // is fatal. It is better to make sure it just doesn't happen.
        // See recv() above for more information.

        let selected_index = self.index();
        match self {
            SelectedOperation::DesyncBufferEntry(index) => {
                crate::log_rr!(Debug, "SelectedOperation::DesyncBufferEntry");
                detthread::inc_event_id();
                Ok(Ok(r.get_buffered_value().expect(
                    "Expected buffer value to be there. This is a bug.",
                )))
            }
            SelectedOperation::Desync(selected) => {
                crate::log_rr!(Debug, "SelectedOperation::Desync");
                detthread::inc_event_id();
                Ok(SelectedOperation::do_recv(selected, r).map(|(_, msg)| msg))
            }
            // Record value we get from direct use of Select API recv().
            SelectedOperation::Record(selected) => {
                crate::log_rr!(Debug, "SelectedOperation::Record");

                let (msg, select_event) = match SelectedOperation::do_recv(selected, r) {
                    // Read value, log information needed for replay later.
                    Ok((sender_thread, msg)) => (
                        Ok(msg),
                        SelectEvent::Success {
                            sender_thread,
                            selected_index,
                        },
                    ),
                    // Err(e) on the RHS is not the same type as Err(e) LHS.
                    Err(e) => (Err(e), SelectEvent::RecvError { selected_index }),
                };
                recordlog::log(
                    RecordedEvent::Select(select_event),
                    r.flavor(),
                    &r.metadata.type_name,
                    &r.metadata.id,
                );
                Ok(msg)
            }
            // We do not use the select API at all on replays. Wait for correct
            // message to come by receving on channel directly.
            // replay_recv takes care of proper buffering.
            SelectedOperation::Replay(event, flavor, chan_id) => {
                // We cannot check these in `Select::select()` since we do not have
                // the receiver until this function to compare values against.
                if flavor != r.flavor() {
                    return Err(DesyncError::ChannelVariantMismatch(flavor, r.flavor()));
                }
                if chan_id != r.metadata.id {
                    return Err(DesyncError::ChannelMismatch(chan_id, r.metadata.id.clone()));
                }

                let retval = match event {
                    SelectEvent::Success { sender_thread, .. } => {
                        crate::log_rr!(Debug, "SelectedOperation::Replay(SelectEvent::Success");
                        Ok(r.replay_recv(sender_thread)?)
                    }
                    SelectEvent::RecvError { .. } => {
                        crate::log_rr!(Debug, "SelectedOperation::Replay(SelectEvent::RecvError");
                        Err(RecvError)
                    }
                };

                detthread::inc_event_id();
                Ok(retval)
            }
            SelectedOperation::NoRR(selected) => {
                // NoRR doesn't need DetThreadId
                Ok(SelectedOperation::do_recv(selected, r).map(|v| v.1))
            }
        }
    }

    // Our channel flavors return slightly different values. Consolidate that here.
    fn do_recv<T>(
        selected: crossbeam_channel::SelectedOperation<'a>,
        r: &crossbeam::Receiver<T>,
    ) -> Result<(Option<DetThreadId>, T), RecvError> {
        match &r.receiver {
            ChannelVariant::After(receiver) => selected
                .recv(receiver)
                .map(|msg| (detthread::get_det_id(), msg)),
            ChannelVariant::Bounded(receiver)
            | ChannelVariant::Unbounded(receiver)
            | ChannelVariant::Never(receiver) => selected.recv(receiver),
        }
    }
}
