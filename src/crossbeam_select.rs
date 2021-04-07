use ::crossbeam_channel as rc; // rc = real_channel
use std::collections::HashMap;

use crate::crossbeam_channel::{self, ChannelKind};
use crate::detthread::DetThreadId;
use crate::error::DesyncError;
use crate::fn_basename;
use crate::recordlog::{self, ChannelVariant, RecordedEvent, SelectEvent};
use crate::rr::DetChannelId;
use crate::DESYNC_MODE;
use crate::{desync, detthread, get_rr_mode, EventRecorder};
use crate::{DesyncMode, DetMessage, RRMode};
#[allow(unused_imports)]
use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

/// Our Select wrapper type must hold different types of Receivet<T>. We use this trait
/// so we can hold different T's via a dynamic trait object.
trait BufferedReceiver {
    /// Returns true if there is any buffered values in this receiver.
    fn buffered_value(&self) -> bool;
    /// Returns true if there is an entry in this receiver waiting to be read.
    /// Returns false if no entry arrives by `timeout`.
    fn poll(&self, sender: &DetThreadId) -> bool;
}

impl<T> BufferedReceiver for crossbeam_channel::Receiver<T> {
    fn buffered_value(&self) -> bool {
        let hashmap = self.buffer.borrow();
        for queue in hashmap.values() {
            if !queue.is_empty() {
                return true;
            }
        }
        false
    }

    fn poll(&self, sender: &DetThreadId) -> bool {
        self.poll_entry(sender)
    }
}

/// Wrapper type around crossbeam's Select.
pub struct Select<'a> {
    selector: rc::Select<'a>,
    mode: RRMode,
    // Use existential to abstract over the `T` which may vary per receiver. We only care
    // if it has entries anyways, not the T contained within.
    receivers: HashMap<usize, &'a dyn BufferedReceiver>,
    event_recorder: EventRecorder,
}

impl<'a> Select<'a> {
    /// Creates an empty list of channel operations for selection.
    pub fn new() -> Select<'a> {
        Select {
            mode: get_rr_mode(),
            selector: rc::Select::new(),
            receivers: HashMap::new(),
            event_recorder: EventRecorder::get_global_recorder(),
        }
    }

    /// Adds a send operation.
    /// Returns the index of the added operation.
    pub fn send<T>(&mut self, _s: &'a crossbeam_channel::Sender<T>) -> usize {
        unimplemented!("Send on select not supported?");
        // log_trace("Select::send()");
        // self.selector.send(&s.sender)
    }

    /// Adds a receive operation.
    /// Returns the index of the added operation.
    pub fn recv<T>(&mut self, r: &'a crossbeam_channel::Receiver<T>) -> usize {
        let _s = self.span(fn_basename!());
        info!("Select adding receiver<{:?}>", r.metadata.id);

        // We still register all receivers with selector, even on replay. As on
        // desynchonization we will have to select on the real selector.
        let index = match &r.receiver {
            ChannelKind::After(r) => self.selector.recv(&r),
            ChannelKind::Bounded(r) | ChannelKind::Unbounded(r) => self.selector.recv(&r),
            ChannelKind::Never(r) => self.selector.recv(&r),
        };
        if self.receivers.insert(index, r).is_some() {
            panic!(
                "Entry already exists at {:?} This should be impossible.",
                index
            );
        }
        index
    }

    pub fn select(&mut self) -> SelectedOperation<'a> {
        let _s = self.span(fn_basename!());

        let inner: SelectedOperationImpl = self.rr_select().unwrap_or_else(|error| {
            warn!("Select: Desynchronization detected! {:?}", error);

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
                            debug!("Buffered value chosen from receiver.");
                            return SelectedOperationImpl::DesyncBufferEntry(*index);
                        }
                    }
                    // No values in buffers, read directly from selector.
                    debug!("No value in buffer, calling select directly.");
                    SelectedOperationImpl::Desync(self.selector.select())
                }
            }
        });

        SelectedOperation::new(inner)
    }

    fn span(&self, fn_name: &str) -> EnteredSpan {
        span!(Level::INFO, stringify!(Select), fn_name).entered()
    }

    pub fn ready(&mut self) -> usize {
        let _s = self.span(fn_basename!());

        self.rr_ready().unwrap_or_else(|error| {
            warn!("Ready::Desynchronization found: {:?}", error);
            match *DESYNC_MODE {
                DesyncMode::Panic => panic!("Desynchronization detected: {:?}", error),
                DesyncMode::KeepGoing => {
                    desync::mark_program_as_desynced();
                    self.selector.ready()
                }
            }
        })
    }

    fn rr_ready(&mut self) -> desync::Result<usize> {
        if desync::program_desyned() {
            let error = DesyncError::Desynchronized;
            error!(%error);
            return Err(error);
        }
        match self.mode {
            RRMode::Record => {
                let select_index = self.selector.ready();
                let metadata = &recordlog::RecordMetadata::new(
                    "ready".to_string(),
                    ChannelVariant::None,
                    DetChannelId::fake(), // We fake it here. We never check this value anyways.
                );

                self.event_recorder
                    .write_event_to_record(RecordedEvent::CbSelectReady { select_index }, metadata);
                Ok(select_index)
            }
            RRMode::Replay => {
                let recorded_event = self.event_recorder.get_log_entry()?;

                match recorded_event.event {
                    RecordedEvent::CbSelectReady { select_index } => Ok(select_index),
                    event => {
                        let dummy = RecordedEvent::CbSelectReady { select_index: 0 };
                        let error = DesyncError::EventMismatch(event, dummy);
                        error!(%error);
                        Err(error)
                    }
                }
            }
            RRMode::NoRR => Ok(self.selector.ready()),
        }
    }

    fn rr_select(&mut self) -> desync::Result<SelectedOperationImpl<'a>> {
        if desync::program_desyned() {
            let error = DesyncError::Desynchronized;
            error!(%error);
            return Err(error);
        }
        match self.mode {
            RRMode::Record => {
                // We don't know the thread_id of sender until the select is complete
                // when user call recv() on SelectedOperation. So do nothing here.
                // Index will be recorded there.
                Ok(SelectedOperationImpl::Record(
                    self.selector.select(),
                    EventRecorder::get_global_recorder(),
                ))
            }
            RRMode::Replay => {
                // Query our log to see what index was selected!() during the replay phase.
                // ChannelVariant type not check on Select::select() but on Select::recv()

                let entry = self.event_recorder.get_log_entry();

                // Here we put this thread to sleep if the entry is missing.
                // On record, the thread never returned from blocking...
                if let Err(e @ DesyncError::NoEntryInLog) = entry {
                    info!("Saw {:?}. Putting thread to sleep.", e);
                    desync::sleep_until_desync();
                    // Thread woke back up... desynced!
                    let error = DesyncError::DesynchronizedWakeup;
                    error!(%error);
                    return Err(error);
                }
                let recorded_entry = entry?;

                match recorded_entry.event {
                    RecordedEvent::CbSelect(event) => {
                        // Verify the receiver has the entry. We check now, since
                        // once we return the SelectedOperation it is too late if it turns
                        // out the message is not there => "unrecoverable error".
                        if let SelectEvent::Success {
                            selected_index,
                            sender_thread,
                        } = event.clone()
                        {
                            debug!("Polling select receiver to see if message is there...");
                            match self.receivers.get(&selected_index) {
                                Some(receiver) => {
                                    if !receiver.poll(&sender_thread) {
                                        debug!("No such message found via polling.");
                                        // We failed to find the message we were expecting
                                        // this is a desynchonization.
                                        let error = DesyncError::Timeout;

                                        error!(%error);
                                        return Err(error);
                                    }
                                    debug!("Polling verified message is present.");
                                }
                                None => {
                                    warn!("Missing receiver.");
                                    let i = selected_index as u64;
                                    let error = DesyncError::MissingReceiver(i);

                                    error!(%error);
                                    return Err(error);
                                }
                            }
                        }

                        Ok(SelectedOperationImpl::Replay(
                            event,
                            recorded_entry.channel_variant,
                            recorded_entry.chan_id,
                        ))
                    }
                    e => {
                        let dummy = SelectEvent::Success {
                            // TODO: Is there a better value to show this is a mock DTI?
                            sender_thread: DetThreadId::new(),
                            selected_index: (0 - 1) as usize,
                        };
                        let error = DesyncError::EventMismatch(e, RecordedEvent::CbSelect(dummy));
                        error!(%error);
                        Err(error)
                    }
                }
            }
            RRMode::NoRR => Ok(SelectedOperationImpl::NoRR(self.selector.select())),
        }
    }
}

impl<'a> Default for Select<'a> {
    fn default() -> Self {
        Select::new()
    }
}

pub struct SelectedOperation<'a> {
    inner: SelectedOperationImpl<'a>,
}

impl<'a> SelectedOperation<'a> {
    fn new(inner: SelectedOperationImpl<'a>) -> Self {
        SelectedOperation { inner }
    }
}

/// True implementation of our SelectedOperation type. We want to hide the fact that this is an
/// enum and keep some our fields private, like `EventRecorder`. So we need this wrapper.
enum SelectedOperationImpl<'a> {
    Replay(SelectEvent, ChannelVariant, DetChannelId),
    /// Desynchonization happened and receiver still has buffered entries. Index
    /// of receiver has been returned to user.
    DesyncBufferEntry(usize),
    /// Record this event. We need somewhere to store the event recorded we plan to write the event
    /// to. We do it here.
    Record(rc::SelectedOperation<'a>, EventRecorder),
    NoRR(rc::SelectedOperation<'a>),
    /// Desynchonization happened, no entries in buffer, we call selector.select()
    /// to have crossbeam do the work for us.
    Desync(rc::SelectedOperation<'a>),
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
        match &self.inner {
            SelectedOperationImpl::DesyncBufferEntry(selected_index)
            | SelectedOperationImpl::Replay(SelectEvent::Success { selected_index, .. }, _, _)
            | SelectedOperationImpl::Replay(SelectEvent::RecvError { selected_index, .. }, _, _) => {
                *selected_index
            }
            SelectedOperationImpl::Record(selected, _)
            | SelectedOperationImpl::NoRR(selected)
            | SelectedOperationImpl::Desync(selected) => selected.index(),
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
    pub fn send<T>(
        self,
        _s: &crossbeam_channel::Sender<T>,
        _msg: T,
    ) -> Result<(), rc::SendError<T>> {
        panic!("Unimplemented send for record and replay channels.")
    }

    fn span(&self, fn_name: &str) -> EnteredSpan {
        span!(Level::INFO, stringify!(SelectedOperation), fn_name).entered()
    }

    /// Completes the receive operation.
    ///
    /// The passed [`Receiver`] reference must be the same one that was used in [`Select::recv`]
    /// when the operation was added.
    ///
    /// # Panics
    ///
    /// Panics if an incorrect [`Receiver`] reference is passed.
    pub fn recv<T>(self, r: &crossbeam_channel::Receiver<T>) -> Result<T, rc::RecvError> {
        let _s = self.span(fn_basename!());

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
                error!(
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
        r: &crossbeam_channel::Receiver<T>,
    ) -> desync::Result<Result<T, rc::RecvError>> {
        // Do not add check for program_desynced() here! This type of desync
        // is fatal. It is better to make sure it just doesn't happen.
        // See recv() above for more information.
        let selected_index = self.index();
        match self.inner {
            SelectedOperationImpl::DesyncBufferEntry(_) => {
                debug!("SelectedOperation::DesyncBufferEntry");
                Ok(Ok(r.get_buffered_value().expect(
                    "Expected buffer value to be there. This is a bug.",
                )))
            }
            SelectedOperationImpl::Desync(selected) => {
                debug!("SelectedOperation::Desync");
                Ok(SelectedOperation::do_recv(selected, r).map(|(_, msg)| msg))
            }
            // Record value we get from direct use of Select API recv().
            SelectedOperationImpl::Record(selected, recorder) => {
                debug!("SelectedOperation::Record");

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
                recorder.write_event_to_record(RecordedEvent::CbSelect(select_event), &r.metadata);
                Ok(msg)
            }
            // We do not use the select API at all on replays. Wait for correct
            // message to come by receving on channel directly.
            // replay_recv takes care of proper buffering.
            SelectedOperationImpl::Replay(event, flavor, chan_id) => {
                // We cannot check these in `Select::select()` since we do not have
                // the receiver until this function to compare values against.
                if flavor != r.metadata.channel_variant {
                    let error =
                        DesyncError::ChannelVariantMismatch(flavor, r.metadata.channel_variant);
                    error!(%error);
                    return Err(error);
                }
                if chan_id != r.metadata.id {
                    let error = DesyncError::ChannelMismatch(chan_id, r.metadata.id.clone());
                    error!(%error);
                    return Err(error);
                }

                let retval = match event {
                    SelectEvent::Success { sender_thread, .. } => {
                        debug!("SelectedOperation::Replay(SelectEvent::Success");
                        Ok(r.replay_recv(&sender_thread)?)
                    }
                    SelectEvent::RecvError { .. } => {
                        debug!("SelectedOperation::Replay(SelectEvent::RecvError");
                        Err(rc::RecvError)
                    }
                };
                Ok(retval)
            }
            SelectedOperationImpl::NoRR(selected) => {
                // NoRR doesn't need DetThreadId
                Ok(SelectedOperation::do_recv(selected, r).map(|v| v.1))
            }
        }
    }

    // Our channel flavors return slightly different values. Consolidate that here.
    fn do_recv<T>(
        selected: rc::SelectedOperation<'a>,
        r: &crossbeam_channel::Receiver<T>,
    ) -> Result<DetMessage<T>, rc::RecvError> {
        match &r.receiver {
            ChannelKind::After(receiver) => selected
                .recv(receiver)
                .map(|msg| (detthread::get_det_id(), msg)),
            ChannelKind::Bounded(receiver)
            | ChannelKind::Unbounded(receiver)
            | ChannelKind::Never(receiver) => selected.recv(receiver),
        }
    }
}
