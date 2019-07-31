use crate::channel::Flavor;
use crate::record_replay::{self, get_log_entry, FlavorMarker, Recorded,
                           SelectEvent, get_log_entry_ret, DesyncError,
                           DetChannelId};
use crate::thread::{get_det_id, get_det_id_desync,
                    get_event_id, inc_event_id, DetThreadId};
use crate::{Receiver, RecordReplayMode, Sender, RECORD_MODE, log_trace, DESYNC_MODE,
            DesyncMode};
use crossbeam_channel::{RecvError, SendError};
use log::{debug, info, trace, warn};

/// Wrapper type around crossbeam's Select.
pub struct Select<'a> {
    selector: crossbeam_channel::Select<'a>,
    mode: RecordReplayMode,
}

impl<'a> Select<'a> {
    /// Creates an empty list of channel operations for selection.
    pub fn new() -> Select<'a> {
        let mode = *RECORD_MODE;
        Select {
            mode,
            selector: crossbeam_channel::Select::new(),
        }
    }

    /// Adds a send operation.
    /// Returns the index of the added operation.
    pub fn send<T>(&mut self, s: &'a Sender<T>) -> usize {
        log_trace("Select::send()");
        self.selector.send(&s.sender)
    }

    /// Adds a receive operation.
    /// Returns the index of the added operation.
    pub fn recv<T>(&mut self, r: &'a Receiver<T>) -> usize {
        log_trace(&format!("Select adding receiver<{:?}>", r.metadata.id));
        // We don't really need this on replay... Just returning a fake "dummy index"
        // would be enough. It still must be the "correct" index otherwise the select!
        // macro will pick the wrong match arm when picking index.
        match &r.receiver {
            Flavor::After(r) => self.selector.recv(&r),
            Flavor::Bounded(r) | Flavor::Unbounded(r) => self.selector.recv(&r),
            Flavor::Never(r) => self.selector.recv(&r),
        }
    }

    pub fn select(&mut self) -> SelectedOperation<'a> {
        log_trace("Select::select()");
        self.record_replay_select().unwrap_or_else(|error| {
            let selected = match &error {
                // We expect this entry never to return on synchronized runs.
                // but on desynchronization, this could lead to deadlocks.
                // So we rely on the blocking behavior of select to
                // validate our assumption.
                // TODO: This is not perfect, it doesn't caputure the case
                // where a blocking select "would have returned" but the
                // program ended just before returning. However, this itself
                // could be considrered a type of nondeterministic race =>
                // between reading from a receiver/select and ending the
                // program. Good programs should do this :)
                e@DesyncError::NoEntryInLog(_, _) => {
                    log_trace(&format!("Missing log entry: {:?}", e));
                    log_trace("Assuming this thread blocked forever.");

                    // Assuming this select never returns! We should
                    // hang here forever...
                    let selected = self.selector.select();
                    warn!("select() with missing log entry returned.");
                    Some(selected)
                }
                _ => None,
            };

            warn!("Desynchronization detected! {:?}", error);
            match *DESYNC_MODE {
                DesyncMode::Panic => {
                    panic!("Desynchronization detected: {:?}", error);
                }
                // Desync for crossbeam select is not so bad. We still register
                // receivers with the selector. So on desync errors we can
                // simply call select() directly.
                DesyncMode::KeepGoing => {
                    SelectedOperation::Desync(
                        selected.unwrap_or_else(|| self.selector.select()))
                }
            }
        })
    }

    pub fn ready(&mut self) -> usize {
        log_trace("Select::ready()");
        self.record_replay_ready().unwrap_or_else(|error| {
            warn!("Desynchronization found: {:?}", error);
            match *DESYNC_MODE {
                DesyncMode::Panic => panic!("Desynchronization detected: {:?}", error),
                DesyncMode::KeepGoing => self.selector.ready(),
            }
        })
    }

    fn record_replay_ready(&mut self) -> Result<usize, DesyncError> {
        match self.mode {
            RecordReplayMode::Record => {
                let select_index = self.selector.ready();
                record_replay::log(
                    Recorded::SelectReady { select_index },
                    FlavorMarker::None,
                    "ready",
                    // Ehh, we fake it here. We never check this value anyways.
                    &DetChannelId::fake()
                );
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
                // We don't know the thread_id of sender until the select is complete
                // when user call recv() on SelectedOperation. So do nothing here.
                // Index will be recorded there.
                Ok(SelectedOperation::Record(self.selector.select()))
            }
            RecordReplayMode::Replay => {
                let det_id = get_det_id_desync()?;
                // Query our log to see what index was selected!() during the replay phase.
                // Flavor type not check on Select::select() but on Select::recv()
                let (event, flavor, chan_id) = get_log_entry_ret(det_id, get_event_id())?;
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
    /// Special variant only seend once a desynchonization has happened.
    Desync(crossbeam_channel::SelectedOperation<'a>),
    Record(crossbeam_channel::SelectedOperation<'a>),
    NoRR(crossbeam_channel::SelectedOperation<'a>),
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
            SelectedOperation::Replay(SelectEvent::Success { selected_index, .. }, _, _)
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
        self.record_replay_recv(r).unwrap_or_else(|error|{
            // This type of desynchronization cannot be recovered from.
            // This is due to how the Select API works for crossbeam channels...
            // By the time we realize we have the "wrong receiver" e.g. flavor
            // of chan_id mismatch, it is too late for us to do the selector.select()
            // event. We cannot do this event preemptively as crossbeam will panic!()
            // if we do select() but do not finish it by doing recv() on the selected
            // operation. This should not really happen. So I think we're okay.
            warn!("Desynchronization detected! {:?}", error);
            panic!("SelectedOperation: Unrecoverable desynchonization: {:?}", error);
        })
    }

    pub fn record_replay_recv<T>(self, r: &Receiver<T>)
                                 -> Result<Result<T, RecvError>, DesyncError> {

        let selected_index = self.index();
        match self {
            SelectedOperation::Desync(selected) => {
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
