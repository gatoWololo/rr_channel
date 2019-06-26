use crossbeam_channel;
use crate::RecordReplayMode;
use log::{trace, debug};
use crossbeam_channel::SendError;
use crate::det_id::*;
use std::collections::HashMap;
use std::collections::VecDeque;
use crossbeam_channel::RecvError;
use crate::ENV_LOGGER;
use crate::RECORD_MODE;
use crate::record_replay::{get_message_and_log, ReceiveType, get_log_entry};
use std::time::Duration;
use std::time::Instant;
use crossbeam_channel::{TryRecvError, RecvTimeoutError};

use std::cell::RefCell;

pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    unimplemented!()
}

pub fn never<T>() -> Receiver<T> {
    unimplemented!()
}

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    // Init log, happens once, lazily.
    // Users always have to make channels before using the rest of this code.
    // So we put it here.
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let mode = *RECORD_MODE;

    (Sender {sender, mode },
     Receiver{buffer: RefCell::new(HashMap::new()), receiver: receiver, mode })
}


pub struct Sender<T>{
    pub(crate) sender: crossbeam_channel::Sender<(Option<DetThreadId>, T)>,
    mode: RecordReplayMode,
}

/// crossbeam_channel::Sender does not derive clone. Instead it implements it,
/// this is to avoid the constraint that T must be Clone.
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender { sender: self.sender.clone(), mode: self.mode.clone() }
    }
}


impl<T> Sender<T> {
    // fn from(sender: crossbeam_channel::Sender<T>) -> Sender<T> {
        // let mode = *RECORD_MODE;
        // Sender { sender, mode }
    // }

    /// Send our det thread id along with the actual message for both
    /// record and replay. On NoRR send None.
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        match self.mode {
            RecordReplayMode::Replay | RecordReplayMode::Record => {
                let det_id = Some(get_det_id());
                trace!("Sending our id via channel: {:?}", det_id);
                // crossbeam::send() returns Result<(), SendError<(DetThreadId, T)>>,
                // we want to make this opaque to the user. Just return the T on error.
                self.sender.send((det_id, msg)).
                    map_err(|e| SendError(e.into_inner().1))
            }
            RecordReplayMode::NoRR => {
                self.sender.send((None, msg)).
                    map_err(|e| SendError(e.into_inner().1))
            }
        }
    }
}

pub struct Receiver<T>{
    /// Buffer holding values from the "wrong" thread on replay mode.
    /// Crossbeam works with inmutable references, so we wrap in a RefCell
    /// to hide our mutation.
    buffer: RefCell<HashMap<DetThreadId, VecDeque<T>>>,
    pub(crate) receiver: crossbeam_channel::Receiver<(Option<DetThreadId>, T)>,
    mode: RecordReplayMode,
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, RecvError> {
        match self.mode {
            RecordReplayMode::Record => {
                let received = self.receiver.recv();
                let msg = get_message_and_log(ReceiveType::DirectChannelRecv, received);
                msg
            }
            RecordReplayMode::Replay => {
                // Query our log to see what index was selected!() during the replay phase.
                let (index, sender_id) = get_log_entry(get_det_id(), get_select_id());
                inc_select_id();

                match index {
                    ReceiveType::Select(index) => {
                        panic!("Expected a ReceiveType::DirectChannelRecv.");
                    }
                    ReceiveType::DirectChannelRecv => {
                        self.replay_recv(&sender_id)
                    }
                }
            }
            RecordReplayMode::NoRR => {
                unimplemented!()
            }
        }
    }

    /// TODO: Move borrow_mut() outside the loop.
    pub fn replay_recv(&self, sender: &Option<DetThreadId>) -> Result<T, RecvError> {
        // Expected an error, just fake it like we got the error.
        if sender.is_none() {
            trace!("User waiting fro RecvError. Created artificial error.");
            return Err(RecvError);
        }

        let sender: &_ = sender.as_ref().unwrap();
        // Check our buffer to see if this value is already here.
        if let Some(entries) = self.buffer.borrow_mut().get_mut(sender) {
            if let Some(entry) = entries.pop_front() {
                debug!("Recv message found in buffer.");
                return Ok(entry);
            }
        }

        // Loop until we get the message we're waiting for. All "wrong" messages are
        // buffered into self.buffer.
        loop {
            match self.receiver.recv() {
                Ok((det_id, msg)) => {
                    let det_id = det_id.
                        expect("None was sent as det_id in record/replay mode!");

                    // We found the message from the expected thread!
                    if det_id == *sender {
                        debug!("Recv message found through recv()");
                        return Ok(msg);
                    }
                    // We got a message from the wrong thread.
                    else {
                        debug!("Wrong value found. Queing value for thread: {:?}", det_id);
                        // ensure we don't drop temporary reference.
                        let mut borrow = self.buffer.borrow_mut();
                        let queue = borrow.entry(det_id).or_insert(VecDeque::new());
                        queue.push_back(msg);
                    }
                }
                Err(_) => {
                    // Got a disconnected message. Ignore.
                }
            }
        }
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        unimplemented!()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        unimplemented!()
    }
}

pub fn after(duration: Duration) -> Receiver<Instant> {
    unimplemented!()
}
