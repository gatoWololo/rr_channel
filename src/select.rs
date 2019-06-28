use crate::{Receiver, Sender, RECORD_MODE, RecordReplayMode};
use crate::det_id::{get_det_id, DetThreadId};
use crate::det_id::get_select_id;
use crate::det_id::inc_select_id;
use crate::record_replay::{self, get_log_entry, RecordedEvent, SelectEvent, ReceiveEvent};
use crossbeam_channel::SendError;
use crossbeam_channel::RecvError;
use log::{debug, trace};


/// Wrapper type around crossbeam's Select.
pub struct Select<'a> {
    selector: crossbeam_channel::Select<'a>,
    mode: RecordReplayMode,
}

impl<'a> Select<'a> {
    /// Creates an empty list of channel operations for selection.
    pub fn new() -> Select<'a> {
        let mode = *RECORD_MODE;
        Select { mode, selector: crossbeam_channel::Select::new()}
    }

    /// Adds a send operation.
    /// Returns the index of the added operation.
    pub fn send<T>(&mut self, s: &'a Sender<T>) -> usize {
        self.selector.send(& s.sender)
    }

    /// Adds a receive operation.
    /// Returns the index of the added operation.
    pub fn recv<T>(&mut self, r: &'a Receiver<T>) -> usize {
        // We don't really need this on replay... Just returning a fake "dummy index"
        // would be enough. It still must be the "correct" index otherwise the select!
        // macro will pick the wrong match arm when picking index.
        self.selector.recv(&r.receiver)
    }

    pub fn select(&mut self) -> SelectedOperation<'a> {
        match self.mode {
            RecordReplayMode::Record => {
                // We don't know the thread_id of sender until the select is complete
                // when user call recv() on SelectedOperation. So do nothing here.
                // Index will be recorded there.
                SelectedOperation::Record(self.selector.select())
            }
            RecordReplayMode::Replay => {
                // Query our log to see what index was selected!() during the replay phase.
                let event = get_log_entry(get_det_id(), get_select_id());
                inc_select_id();

                match event {
                    RecordedEvent::Select(select_entry) => {
                        SelectedOperation::Replay(select_entry)
                    }
                    e => panic!("Unexpected event entry from replay select: {:?}", e),
                }
            }
            RecordReplayMode::NoRR => {
                SelectedOperation::Record(self.selector.select())
            }
        }
    }

    pub fn ready(&mut self) -> usize {
        match self.mode {
            RecordReplayMode::Record => {
                let select_index = self.selector.ready();
                record_replay::log(RecordedEvent::SelectReady{ select_index });
                select_index
            }
            RecordReplayMode::Replay => {
                let event = get_log_entry(get_det_id(), get_select_id());
                inc_select_id();

                match event {
                    RecordedEvent::SelectReady{select_index} => {
                        *select_index
                    }
                    e => panic!("Unexpected event RecordedEvent from ready(): {:?}", e),
                }
            }
            RecordReplayMode::NoRR => {
                unimplemented!()
            }
        }
    }
}

pub enum SelectedOperation<'a> {
    Replay(&'a SelectEvent),
    Record(crossbeam_channel::SelectedOperation<'a>),
    // TODO add variant for "no record"!!!
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
            SelectedOperation::Replay(SelectEvent::Success{ selected_index, .. }) => {
                *selected_index
            }
            SelectedOperation::Replay(SelectEvent::RecvError{ selected_index, .. }) => {
                *selected_index
            }
            SelectedOperation::Record(selected) =>{
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
        // s.send(msg)
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
        let selected_index = self.index();
        match self {
            // Record value we get from direct use of Select API
            SelectedOperation::Record(selected) => {
                match selected.recv(&r.receiver) {
                    // Disconnected channel returned RecvError.
                    Err(e) => {
                        let event = RecordedEvent::Select(
                            SelectEvent::RecvError {selected_index});
                        record_replay::log(event);
                        // Err(e) on the RHS is not the same type as Err(e) LHS.
                        Err(e)
                    }
                    // Read value, log information needed for replay later.
                    Ok((sender_thread, msg)) => {
                        let event = RecordedEvent::Select(
                            SelectEvent::Success { sender_thread, selected_index });
                        record_replay::log(event);
                        Ok(msg)
                    }
                }
            }
            // We do not use the select API at all on replays. Wait for correct
            // message to come by receving on channel directly.
            // replay_recv takes care of proper buffering.
            SelectedOperation::Replay(SelectEvent::Success{ sender_thread, .. }) => {
                Ok(r.replay_recv(sender_thread))
            }
            SelectedOperation::Replay(SelectEvent::RecvError{..}) => {
                Err(RecvError)
            }
        }
    }
}
