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

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    // Init log, happens once, lazily.
    // Users always have to make channels before using the rest of this code.
    // So we put it here.
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let mode = *RECORD_MODE;

    (Sender {sender, mode },
     Receiver{buffer: HashMap::new(), receiver, mode })
}


#[derive(Clone)]
pub struct Sender<T>{
    pub sender: crossbeam_channel::Sender<(Option<DetThreadId>, T)>,
    mode: RecordReplayMode,
}


impl<T> Sender<T> {
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        match self.mode {
            RecordReplayMode::Replay | RecordReplayMode::Record => {
                let det_id = Some(get_det_id());
                trace!("Sending our id via channel: {:?}", det_id);
                self.sender.send((det_id, msg)).
                // crossbeam.send() returns Result<(), SendError<(DetThreadId, T)>>,
                // we want to make this opaque to the user. Just return the T on error.
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
    // /// Should be set before calling recv(). Will panic if not set.
    // /// We do not return an error to keep apperances with crossbeam_channels
    // /// API.
    // expected_id: Option<DetThreadId>,
    /// We could buffer multiple values for a given thread.
    buffer: HashMap<DetThreadId, VecDeque<T>>,
    pub receiver: crossbeam_channel::Receiver<(Option<DetThreadId>, T)>,
    mode: RecordReplayMode,
}

impl<T> Receiver<T> {
    // pub fn set_expected_id(&mut self, expected_id: DetThreadId) {
    //     if self.mode != RecordReplayMode::Replay {
    //         panic!("Expected id only makes sense in Replay mode");
    //     }
    //     self.expected_id = Some(expected_id);
    // }
    pub fn recv(&mut self) -> Result<(DetThreadId, T), RecvError> {
        if self.mode != RecordReplayMode::Record {
            panic!("record_recv should only be called in record mode.");
        }
        self.receiver.recv().
            map(|(id, msg)| (id.expect("None sent through channel on record."), msg))
    }

    pub fn replay_recv(&mut self, sender: &Option<DetThreadId>) -> Result<T, RecvError> {
        // Expected an error, just fake it like we got the error.
        if sender.is_none() {
            trace!("User waiting fro RecvError. Created artificial error.");
            return Err(RecvError);
        }

        let sender: &_ = sender.as_ref().unwrap();
        // Check our buffer to see if this value is already here.
        if let Some(entries) = self.buffer.get_mut(sender) {
            if let Some(entry) = entries.pop_front() {
                debug!("Recv message found in buffer.");
                return Ok(entry);
            }
        }

        // Nope, keep receiving until we find it.
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
                        let queue = self.buffer.entry(det_id).or_insert(VecDeque::new());
                        queue.push_back(msg);
                    }
                }
                Err(_) => {
                    // Got a disconnected message. Ignore.
                }
            }
        }
    }
}
