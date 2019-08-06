use crate::channel::Flavor;
use crate::record_replay::{self, get_log_entry, FlavorMarker, Recorded,
                           SelectEvent, get_log_entry_ret, DesyncError,
                           DetChannelId, BlockingOp};
use crate::thread::{get_det_id, get_det_id_desync,
                    get_event_id, inc_event_id, DetThreadId};
use crate::{Receiver, RecordReplayMode, Sender, RECORD_MODE, log_trace, DESYNC_MODE,
            DesyncMode};
use crossbeam_channel::{RecvError, SendError};
use log::{debug, info, trace, warn};
use std::collections::HashMap;
use std::thread;

trait BufferedReceiver {
    /// Returns true if there is any buffered values in this receiver.
    fn buffered_value(&self) -> bool;
}

impl<T> BufferedReceiver for Receiver<T> {
    fn buffered_value(&self) -> bool {
        let hashmap = self.buffer.borrow();
        for queue in hashmap.values() {
            if ! queue.is_empty() {
                return true;
            }
        }
        return false;
    }
}

/// Wrapper type around crossbeam's Select.
pub struct Select<'a> {
    selector: crossbeam_channel::Select<'a>,
    mode: RecordReplayMode,
    // Use existential to abstract over the `T` which may vary
    // per receiver. We only care if it has entries anyways.
    receivers: HashMap<usize, &'a dyn BufferedReceiver>,
}

impl<'a> Select<'a> {
    /// Creates an empty list of channel operations for selection.
    pub fn new() -> Select<'a> {
        let mode = *RECORD_MODE;
        Select {
            mode,
            selector: crossbeam_channel::Select::new(),
            receivers: HashMap::new()
        }
    }

    /// Adds a send operation.
    /// Returns the index of the added operation.
    pub fn send<T>(&mut self, s: &'a Sender<T>) -> usize {
        unimplemented!("Send on select not supported?");
        // log_trace("Select::send()");
        // self.selector.send(&s.sender)
    }

    /// Adds a receive operation.
    /// Returns the index of the added operation.
    pub fn recv<T>(&mut self, r: &'a Receiver<T>) -> usize {
        log_trace(&format!("Select adding receiver<{:?}>", r.metadata.id));

        // We still register all receivers with selector, even on replay. As on
        // desynchonization we will have to select on the real selector.
        let index = match &r.receiver {
            Flavor::After(r) => self.selector.recv(&r),
            Flavor::Bounded(r) | Flavor::Unbounded(r) => self.selector.recv(&r),
            Flavor::Never(r) => self.selector.recv(&r),
        };
        if let Some(v) = self.receivers.insert(index, r) {
            panic!("Entry already exists at {:?} This should be impossible.", index);
        }
        index
    }

    pub fn select(&mut self) -> SelectedOperation<'a> {
        log_trace("Select::select()");
        self.record_replay_select().unwrap_or_else(|error| {
            warn!("Select: Desynchronization detected! {:?}", error);
            log_trace(&format!("Select: Desynchronization detected! {:?}", error));

            match *DESYNC_MODE {
                DesyncMode::Panic => {
                    panic!("Select: Desynchronization detected: {:?}", error);
                }
                // Desync for crossbeam select is not so bad. We still register
                // receivers with the selector. So on desync errors we can
                // simply call select() directly, we check if any receiver's buffer
                // has values which we could "flush" first...
                DesyncMode::KeepGoing => {
                    for (index, receiver) in self.receivers.iter() {
                        if receiver.buffered_value() {
                            log_trace("Buffered value chosen from receiver.");
                            return SelectedOperation::DesyncBufferEntry(*index);
                        }
                    }
                    // No values in buffers, read directly from selector.
                    log_trace("No value in buffer, calling select directly.");
                    SelectedOperation::Desync(self.selector.select())
                }
            }
        })
    }

    pub fn ready(&mut self) -> usize {
        log_trace("Select::ready()");
        self.record_replay_ready().unwrap_or_else(|error| {
            warn!("Ready::Desynchronization found: {:?}", error);
            match *DESYNC_MODE {
                DesyncMode::Panic => panic!("Desynchronization detected: {:?}", error),
                DesyncMode::KeepGoing => {
                    inc_event_id();
                    self.selector.ready()
                }
            }
        })
    }

    fn record_replay_ready(&mut self) -> Result<usize, DesyncError> {
        match self.mode {
            RecordReplayMode::Record => {
                let select_index = self.selector.ready();
                record_replay::log(Recorded::SelectReady { select_index },
                                   FlavorMarker::None,
                                   "ready",
                                   // Ehh, we fake it here. We never check this value anyways.
                                   &DetChannelId::fake());
                Ok(select_index)
            }
            RecordReplayMode::Replay => {
                let det_id = get_det_id_desync()?;
                let event = get_log_entry(det_id, get_event_id())?;
                inc_event_id();

                match event {
                    Recorded::SelectReady { select_index } => Ok(*select_index),
                    event => {
                        let dummy = Recorded::SelectReady{ select_index: 0 };
                        Err(DesyncError::EventMismatch(event.clone(), dummy))
                    }
                }
            }
            RecordReplayMode::NoRR => Ok(self.selector.ready()),
        }
    }

    fn record_replay_select(&mut self) -> Result<SelectedOperation<'a>, DesyncError> {
        match self.mode {
            RecordReplayMode::Record => {
                // Log the fact that we started a possibly blocking operation!
                let record = Recorded::BlockingOpStart(BlockingOp::Select);
                record_replay::log(record, FlavorMarker::Select,
                                   "crossbeam_channel::Select", &DetChannelId::fake());
                // We don't know the thread_id of sender until the select is complete
                // when user call recv() on SelectedOperation. So do nothing here.
                // Index will be recorded there.
                Ok(SelectedOperation::Record(self.selector.select()))
            }
            RecordReplayMode::Replay => {
                let det_id = get_det_id_desync()?;

                // We expect to see a start message first.
                let (event, _, _) = get_log_entry_ret(det_id.clone(), get_event_id())?;
                match event {
                    // Ensure event is a BlockingOpStart
                    Recorded::BlockingOpStart(_ /*TODO*/) => { }
                    event => {
                        let dummy = Recorded::BlockingOpStart(BlockingOp::Recv);
                        let e = DesyncError::EventMismatch(dummy, event.clone());
                        return Err(e);
                    }
                }
                inc_event_id();

                // Query our log to see what index was selected!() during the replay phase.
                // Flavor type not check on Select::select() but on Select::recv()
                let entry = get_log_entry_ret(det_id, get_event_id());

                // Here we put this thread to sleep if the entry is missing.
                // On record, the thread never returned from blocking...
                if let Err(e@DesyncError::NoEntryInLog(_, _)) = entry {
                    log_trace(&format!("Saw {:?}. Putting thread to sleep.", e));
                    loop { thread::park() }
                }

                let (event, flavor, chan_id) = entry?;
                match event {
                    Recorded::Select(select_entry) => {
                        Ok(SelectedOperation::Replay(select_entry, *flavor, chan_id.clone()))
                    }
                    e => {
                        let dummy = SelectEvent::Success { sender_thread: None,
                                                           selected_index: (0 - 1) as usize};
                        Err(DesyncError::EventMismatch(e.clone(), Recorded::Select(dummy)))
                    }
                }
            }
            RecordReplayMode::NoRR => {
                Ok(SelectedOperation::NoRR(self.selector.select()))
            }
        }
    }
}

pub enum SelectedOperation<'a> {
    Replay(&'a SelectEvent, FlavorMarker, DetChannelId),
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
            SelectedOperation::Record(selected) |
            SelectedOperation::NoRR(selected)   |
            SelectedOperation::Desync(selected) => {
                selected.index()
            }
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
    pub fn send<T>(self, _s: &Sender<T>, _msg: T) -> Result<(), SendError<T>> {
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
    pub fn recv<T>(self, r: &Receiver<T>) -> Result<T, RecvError> {
        log_trace(&format!("SelectedOperation<{:?}>::recv()", r.metadata().id));

        match self.record_replay_select_recv(r) {
            Ok(v) => v,
            Err(error) => {
                // This type of desynchronization cannot be recovered from.
                // This is due to how the Select API works for crossbeam channels...
                // By the time we realize we have the "wrong receiver" e.g. flavor
                // of chan_id mismatch, it is too late for us to do the selector.select()
                // event. We cannot do this event preemptively as crossbeam will panic!()
                // if we do select() but do not finish it by doing recv() on the selected
                // operation. This should not really happen. So I think we're okay.
                warn!("Desynchronization detected! {:?}", error);
                panic!("SelectedOperation: Unrecoverable desynchonization: {:?}", error);
            }
        }
    }

    pub fn record_replay_select_recv<T>(self, r: &Receiver<T>)
                                 -> Result<Result<T, RecvError>, DesyncError> {
        let selected_index = self.index();
        match self {
            SelectedOperation::DesyncBufferEntry(index) => {
                log_trace("SelectedOperation::DesyncBufferEntry");
                Ok(Ok(r.get_buffered_value().
                    expect("Expected buffer value to be there. This is a bug.")))
            }
            SelectedOperation::Desync(selected) => {
                log_trace("SelectedOperation::Desync");
                Ok(SelectedOperation::do_recv(selected, r).map(|(_, msg)| msg))
            }
            // Record value we get from direct use of Select API recv().
            SelectedOperation::Record(selected) => {
                log_trace("SelectedOperation::Record");

                let (msg, select_event) = match SelectedOperation::do_recv(selected, r) {
                    // Read value, log information needed for replay later.
                    Ok((sender_thread, msg)) => {
                        (Ok(msg), SelectEvent::Success { sender_thread, selected_index })
                    }
                    // Err(e) on the RHS is not the same type as Err(e) LHS.
                    Err(e) => {
                        (Err(e), SelectEvent::RecvError { selected_index })
                    }
                };
                record_replay::log(Recorded::Select(select_event),
                                   r.flavor(),
                                   &r.metadata.type_name,
                                   &r.metadata.id);
                Ok(msg)
            }
            // We do not use the select API at all on replays. Wait for correct
            // message to come by receving on channel directly.
            // replay_recv takes care of proper buffering.
            SelectedOperation::Replay(event, flavor, chan_id) => {
                // We cannot check these in `Select::select()` since we do not have
                // the receiver until this function to compare values against.
                if flavor != r.flavor() {
                    return Err(DesyncError::FlavorMismatch(flavor, r.flavor()));
                }
                if chan_id != r.metadata.id {
                    return Err(DesyncError::ChannelMismatch(chan_id, r.metadata.id.clone()));
                }

                let retval = match event {
                    SelectEvent::Success { sender_thread, .. } => {
                        log_trace("SelectedOperation::Replay(SelectEvent::Success");
                        Ok(r.replay_recv(sender_thread))
                    }
                    SelectEvent::RecvError { .. } => {
                        log_trace("SelectedOperation::Replay(SelectEvent::RecvError");
                        Err(RecvError)
                    }
                };

                inc_event_id();
                Ok(retval)
            }
            SelectedOperation::NoRR(selected) => {
                // NoRR doesn't need DetThreadId
                Ok(SelectedOperation::do_recv(selected, r).map(|v| v.1))
            }
        }
    }

    // Our channel flavors return slightly different values. Consolidate that here.
    fn do_recv<T>(selected: crossbeam_channel::SelectedOperation<'a>,
                  r: &Receiver<T>)
                  -> Result<(Option<DetThreadId>, T), RecvError>{
        match &r.receiver {
            Flavor::After(receiver) => {
                selected.recv(receiver).map(|msg| (get_det_id(), msg))
            }
            Flavor::Bounded(receiver)
                | Flavor::Unbounded(receiver)
                | Flavor::Never(receiver) => selected.recv(receiver)
        }
    }
}
