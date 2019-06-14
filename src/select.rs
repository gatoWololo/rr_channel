use crate::{Receiver, Sender, RECORD_MODE, RecordReplayMode};
use crate::det_id::{get_det_id, DetThreadId};
use crate::det_id::get_select_id;
use crate::record_replay::LogEntry;
use crate::record_replay::WRITE_LOG_FILE;
use crate::det_id::inc_select_id;
use std::io::Write;
use crate::record_replay::RECORDED_INDICES;

use crossbeam_channel::SendError;
use crossbeam_channel::RecvError;
use log::{debug, trace};

/// Wrapper type around crossbeam's Select.
pub struct Select<'a> {
    selector: crossbeam_channel::Select<'a>,
    mode: RecordReplayMode,
    // Keep track of how many channel ends have been "added".
    // TODO may not be needed at all.
    index: usize,
}

impl<'a> Select<'a> {
    /// Creates an empty list of channel operations for selection.
    pub fn new() -> Select<'a> {
        let mode = *RECORD_MODE;
        Select { mode, selector: crossbeam_channel::Select::new(), index: 0}
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
                let key = (get_det_id(), get_select_id());
                trace!("Replaying for our_thread: {:?}, sender_thread {:?}",
                       key.0, key.1);
                let (index, sender_id) = RECORDED_INDICES.get(& key).
                    expect("Unable to fetch key.");

                trace!("Index fetched: {}", index);
                trace!("Sender Id fetched: {:?}", sender_id);

                inc_select_id();
                SelectedOperation::Replay(
                    ReplaySelectedOperation {index: *index, expected_thread: sender_id})
            }
            RecordReplayMode::NoRR => {
                SelectedOperation::Record(self.selector.select())
            }
        }
    }

    pub fn ready(&mut self) -> usize {
        unimplemented!()
    }
}

/// "Fake" selected operation used in replay.
pub struct ReplaySelectedOperation<'a> {
    index: u32,
    expected_thread: &'a Option<DetThreadId>,
}

pub enum SelectedOperation<'a> {
    /// Holds the DetThreadId we're waiting for. Or None if we're expecting
    /// a disconnect errror.
    Replay(ReplaySelectedOperation<'a>),
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
    pub fn index(&self) -> usize {
        match self {
            SelectedOperation::Replay(dummy_select) => {
                dummy_select.index as usize
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
        let index = self.index() as u32;
        match self {
            // We do not use the select API at all on replays. Wait for correct
            // message to come by receving on channel directly.
            // replay_recv  // takes care of proper buffering.
            SelectedOperation::Replay(dummy_select) => {
                r.replay_recv(dummy_select.expected_thread)
            }

            // Record value we get from direct use of Select API
            SelectedOperation::Record(selected) => {
                let (sender_thread, ret_val) = match selected.recv(&r.receiver) {
                    Err(e) => (None, Err(e)),
                    Ok((sender_thread, msg)) => (sender_thread, Ok(msg)),
                };

                // Write our (DET_ID, SELECT_ID) -> index to our log file
                let current_thread = get_det_id();
                let select_id = get_select_id();
                let entry: LogEntry = LogEntry { current_thread, sender_thread,
                                                 select_id , index };
                let serialized = serde_json::to_string(&entry).unwrap();

                WRITE_LOG_FILE.
                    lock().
                    unwrap().
                    write_fmt(format_args!("{}\n", serialized)).
                    expect("Unable to write to log file.");

                debug!("Logged entry: {:?}", entry);
                inc_select_id();
                ret_val
            }
        }
    }
}
