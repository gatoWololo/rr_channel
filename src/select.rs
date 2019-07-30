use crate::channel::Flavor;
use crate::log_trace;
use crate::record_replay::DetChannelId;
use crate::record_replay::{self, get_log_entry, FlavorMarker, Recorded, SelectEvent};
use crate::thread::get_det_id;
use crate::thread::get_event_id;
use crate::thread::inc_event_id;
use crate::{Receiver, RecordReplayMode, Sender, RECORD_MODE};
use crossbeam_channel::RecvError;
use crossbeam_channel::SendError;
use log::{debug, info, trace, warn};
use crate::thread::DetThreadId;

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
        match self.mode {
            RecordReplayMode::Record => {
                // We don't know the thread_id of sender until the select is complete
                // when user call recv() on SelectedOperation. So do nothing here.
                // Index will be recorded there.
                SelectedOperation::Record(self.selector.select())
            }
            RecordReplayMode::Replay => {
                let det_id = get_det_id().unwrap_or_else(|| {
                    panic!("select(): get_det_id called from outside thread context")
                });
                // Query our log to see what index was selected!() during the replay phase.
                // Flavor type not check on Select::select() but on Select::recv()
                match get_log_entry(det_id, get_event_id()) {
                    Some((event, flavor, chan_id)) => match event {
                        Recorded::Select(select_entry) => {
                            SelectedOperation::Replay(select_entry, *flavor, chan_id.clone())
                        }
                        e => {
                            log_trace(&format!("Unexpected Recorded at Select::select(): {:?}", e));
                            panic!("Unexpected Recorded at Select::select(): {:?}", e);
                        }
                    },
                    None => {
                        log_trace(
                            "No entry in log. Assuming this thread blocked on this \
                             select forever.",
                        );
                        // No entry in log. This means that this event waiting forever
                        // on select... Do the same here.
                        loop {
                            std::thread::park();
                            log_trace("Spurious wakeup, going back to sleep.");
                        }
                    }
                }
            }
            RecordReplayMode::NoRR => SelectedOperation::NoRR(self.selector.select()),
        }
    }

    pub fn ready(&mut self) -> usize {
        log_trace("Select::ready()");

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
                select_index
            }
            RecordReplayMode::Replay => {
                let det_id = get_det_id().expect("ready(): get_det_id called outside thread");
                // No channel flavor.
                let (event, _, _) =
                    get_log_entry(det_id, get_event_id()).expect("No such key in map.");
                inc_event_id();
                match event {
                    Recorded::SelectReady { select_index } => *select_index,
                    e => panic!("Unexpected event Recorded from ready(): {:?}", e),
                }
            }
            RecordReplayMode::NoRR => self.selector.ready(),
        }
    }
}

pub enum SelectedOperation<'a> {
    Replay(&'a SelectEvent, FlavorMarker, DetChannelId),
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
            SelectedOperation::Record(selected) | SelectedOperation::NoRR(selected) => {
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
        let selected_index = self.index();

        match self {
            // Record value we get from direct use of Select API recv().
            SelectedOperation::Record(selected) => {
                log_trace("SelectedOperation::Record");

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

                record_replay::log(
                    Recorded::Select(select_event),
                    r.flavor(),
                    &r.metadata.type_name,
                    &r.metadata.id,
                );
                msg
            }
            // We do not use the select API at all on replays. Wait for correct
            // message to come by receving on channel directly.
            // replay_recv takes care of proper buffering.
            SelectedOperation::Replay(event, flavor, chan_id) => {
                // We cannot check these in `Select::select()` since we do not have
                // the receiver until this function to compare values against.
                if flavor != r.flavor() {
                    panic!("Expected {:?}, saw {:?}", flavor, r.flavor());
                }
                if chan_id != r.metadata.id {
                    panic!("Expected {:?}, saw {:?}", chan_id, r.metadata.id);
                }

                match event {
                    SelectEvent::Success { sender_thread, .. } => {
                        log_trace("SelectedOperation::Replay(SelectEvent::Success");
                        let retval = r.replay_recv(sender_thread);
                        inc_event_id();
                        Ok(retval)
                    }
                    SelectEvent::RecvError { .. } => {
                        log_trace("SelectedOperation::Replay(SelectEvent::RecvError");
                        inc_event_id();
                        Err(RecvError)
                    }
                }
            }
            SelectedOperation::NoRR(selected) => {
                // NoRR doesn't need DetThreadId
                SelectedOperation::do_recv(selected, r).map(|v| v.1)
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
