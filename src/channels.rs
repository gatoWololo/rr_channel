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
use crate::record_replay::{self, RecordedEvent, ReceiveEvent, TryRecvEvent,
                           RecvTimeoutEvent, FlavorMarker};
use std::time::Duration;
use std::time::Instant;
use crossbeam_channel::{TryRecvError, RecvTimeoutError};

use std::cell::RefCell;

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    // Init log, happens once, lazily.
    // Users always have to make channels before using the rest of this code.
    // So we put it here.
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let receiver = Flavor::Unbounded(receiver);
    let mode = *RECORD_MODE;

    (Sender {sender, mode },
     Receiver{buffer: RefCell::new(HashMap::new()), receiver, mode })
}


pub struct Sender<T>{
    pub(crate) sender: crossbeam_channel::Sender<(DetThreadId, T)>,
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
    /// Send our det thread id along with the actual message for both
    /// record and replay. On NoRR send None.
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        // We send the det_id even when running in RecordReplayMode::NoRR,
        // but that's okay. It makes logic a little simpler.
        let det_id = get_det_id();
        trace!("Sending our id via channel: {:?}", det_id);

        // crossbeam::send() returns Result<(), SendError<(DetThreadId, T)>>,
        // we want to make this opaque to the user. Just return the T on error.
        self.sender.send((det_id, msg)).
            map_err(|e| SendError(e.into_inner().1))
    }
}

/// Holds different channels types which may have slightly different types.
/// This is just the complexity cost of piggybacking off crossbeam_channels
/// and their API so much.
pub enum Flavor<T> {
    After(crossbeam_channel::Receiver<T>),
    Bounded(crossbeam_channel::Receiver<(DetThreadId, T)>),
    Never(crossbeam_channel::Receiver<(DetThreadId, T)>),
    Unbounded(crossbeam_channel::Receiver<(DetThreadId, T)>),
}

/// Helper macro to generate methods for Flavor. Calls appropriate channel method.
/// On case of Flavor::After inserts get_det_id() as the sender_thread_id in order
/// to have the correct type.
macro_rules! generate_channel_method {
    ($method:ident, $error_type:ty) => {
        pub fn $method(&self) -> Result<(DetThreadId, T), $error_type> {
            match self {
                Flavor::After(receiver) => {
                    match receiver.$method() {
                        Ok(msg) => Ok((get_det_id(), msg)),
                        e => e.map(|_| unreachable!()),
                    }
                }
                Flavor::Bounded(receiver) | Flavor::Unbounded(receiver) =>
                    receiver.$method(),
                Flavor::Never(receiver) => receiver.$method(),
            }
        }
    }
}

impl<T> Flavor<T> {
    generate_channel_method!(recv, RecvError);
    generate_channel_method!(try_recv, TryRecvError);

    pub fn recv_timeout(&self, duration: Duration) ->
        Result<(DetThreadId, T), RecvTimeoutError> {
            match self {
                Flavor::After(receiver) => {
                    match receiver.recv_timeout(duration) {
                        Ok(msg) => Ok((get_det_id(), msg)),
                        e => e.map(|_| unreachable!()),
                    }
                }
                Flavor::Bounded(receiver) => receiver.recv_timeout(duration),
                Flavor::Never(receiver) => receiver.recv_timeout(duration),
                Flavor::Unbounded(receiver) => receiver.recv_timeout(duration),

            }
        }
}

pub struct Receiver<T>{
    /// Buffer holding values from the "wrong" thread on replay mode.
    /// Crossbeam works with inmutable references, so we wrap in a RefCell
    /// to hide our mutation.
    buffer: RefCell<HashMap<DetThreadId, VecDeque<T>>>,
    pub(crate) receiver: Flavor<T>,
    mode: RecordReplayMode,
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, RecvError> {
        match self.mode {
            RecordReplayMode::Record => {
                let (result, event) = match self.receiver.recv() {
                    // Err(e) on the RHS is not the same type as Err(e) LHS.
                    Err(e) => (Err(e), ReceiveEvent::RecvError),

                    // Read value, log information needed for replay later.
                    Ok((sender_thread, msg)) => {
                        (Ok(msg), ReceiveEvent::Success { sender_thread })
                    }
                };
                record_replay::log(RecordedEvent::Receive(event), self.flavor());
                result
            }

            RecordReplayMode::Replay => {
                let (event, flavor) =
                    record_replay::get_log_entry(get_det_id(), get_select_id());
                inc_select_id();

                if flavor != self.flavor() {
                    panic!("Expected {:?}, saw {:?}", flavor, self.flavor());
                }

                match event {
                    RecordedEvent::Receive(
                        ReceiveEvent::Success{sender_thread}) => {
                        Ok(self.replay_recv(&sender_thread))
                    }
                    RecordedEvent::Receive(ReceiveEvent::RecvError) => {
                        if flavor != self.flavor() {
                            panic!("Expected {:?}, saw {:?}", flavor, self.flavor());
                        }
                        trace!("Saw RecvError on record, creating artificial one.");
                        return Err(RecvError);
                    }
                    _ => panic!("Unexpected event: {:?} in replay for recv()"),
                }
            }
            RecordReplayMode::NoRR => {
                unimplemented!()
            }
        }
    }

    /// TODO: Move borrow_mut() outside the loop.
    pub fn replay_recv(&self, sender: &DetThreadId) -> T {
        // Check our buffer to see if this value is already here.
        if let Some(entries) = self.buffer.borrow_mut().get_mut(sender) {
            if let Some(entry) = entries.pop_front() {
                debug!("Recv message found in buffer.");
                return entry;
            }
        }

        // Loop until we get the message we're waiting for. All "wrong" messages are
        // buffered into self.buffer.
        loop {
            match self.receiver.recv() {
                Ok((det_id, msg)) => {
                    // We found the message from the expected thread!
                    if det_id == *sender {
                        debug!("Recv message found through recv()");
                        return msg;
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
        match self.mode {
            // Call crossbeam_channel::try_recv() directly and record results.
            RecordReplayMode::Record => {
                let (result, event) = match self.receiver.try_recv() {
                    Err(TryRecvError::Disconnected) => {
                        (Err(TryRecvError::Disconnected), TryRecvEvent::Disconnected)
                    }
                    Err(TryRecvError::Empty) => {
                        (Err(TryRecvError::Empty), TryRecvEvent::Empty)
                    }
                    Ok((sender_thread, msg)) => {
                        (Ok(msg), TryRecvEvent::Success { sender_thread })
                    }
                };
                record_replay::log(RecordedEvent::TryRecv(event), self.flavor());
                result
            }
            RecordReplayMode::Replay => {
                let (event, flavor) =
                    record_replay::get_log_entry(get_det_id(), get_select_id());
                inc_select_id();

                if flavor != self.flavor() {
                    panic!("Expected {:?}, saw {:?}", flavor, self.flavor());
                }

                match event {
                    RecordedEvent::TryRecv(TryRecvEvent::Success{sender_thread}) => {
                        Ok(self.replay_recv(&sender_thread))
                    }
                    RecordedEvent::TryRecv(TryRecvEvent::Empty) => {
                        trace!("Record had TryRecvError::Empty, creating artificial one.");
                        return Err(TryRecvError::Empty);
                    }
                    RecordedEvent::TryRecv(TryRecvEvent::Disconnected) => {
                        trace!("Record had TryRecvError::Disconnected, creating \
                                artificial one.");
                        return Err(TryRecvError::Disconnected);
                    }
                    _ => panic!("Unexpected event: {:?} in replay for try_recv()"),
                }
            }
            RecordReplayMode::NoRR => {
                unimplemented!()
            }
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        match self.mode {
            // Call crossbeam_channel method and record results.
            RecordReplayMode::Record => {
                let (result, event) = match self.receiver.recv_timeout(timeout) {
                    Err(error) => {
                        let event = match error {
                            RecvTimeoutError::Disconnected => RecvTimeoutEvent::Disconnected,
                            RecvTimeoutError::Timeout => RecvTimeoutEvent::Timedout
                        };
                        (Err(error), event)
                    }
                    Ok((sender_thread, msg)) => {
                        (Ok(msg), RecvTimeoutEvent::Success{ sender_thread })
                    }
                };
                record_replay::log(RecordedEvent::RecvTimeout(event), self.flavor());
                result
            }
            RecordReplayMode::Replay => {
                let (event, flavor) =
                    record_replay::get_log_entry(get_det_id(), get_select_id());
                inc_select_id();

                if flavor != self.flavor() {
                    panic!("Expected {:?}, saw {:?}", flavor, self.flavor());
                }

                match event {
                    RecordedEvent::RecvTimeout(RecvTimeoutEvent::Success{sender_thread}) => {
                        Ok(self.replay_recv(&sender_thread))
                    }
                    RecordedEvent::RecvTimeout(RecvTimeoutEvent::Timedout) => {
                        trace!("Record had RecvTimeoutError::Timeout, creating \
                                artificial one.");
                        return Err(RecvTimeoutError::Timeout);
                    }
                    RecordedEvent::RecvTimeout(RecvTimeoutEvent::Disconnected) => {
                        trace!("Record had RecvTimeoutError::Disconnected, creating \
                                artificial one.");
                        return Err(RecvTimeoutError::Disconnected);
                    }
                    _ => panic!("Unexpected event: {:?} in replay for try_recv()"),
                }
            }
            RecordReplayMode::NoRR => {
                unimplemented!()
            }
        }
    }

    pub(crate) fn flavor(&self) -> FlavorMarker {
        match self.receiver {
            Flavor::Unbounded(_) => FlavorMarker::Unbounded,
            Flavor::After(_) => FlavorMarker::After,
            Flavor::Bounded(_) => FlavorMarker::Bounded,
            Flavor::Never(_) => FlavorMarker::Never,
        }
    }
}

pub fn after(duration: Duration) -> Receiver<Instant> {
    // Init log, happens once, lazily.
    // Users always have to make channels before using the rest of this code.
    // So we put it here.
    *ENV_LOGGER;
    let mode = *RECORD_MODE;

    Receiver{buffer: RefCell::new(HashMap::new()),
             receiver: Flavor::After(crossbeam_channel::after(duration)),
             mode }
}

pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    unimplemented!()
}

pub fn never<T>() -> Receiver<T> {
    // Init log, happens once, lazily.
    // Users always have to make channels before using the rest of this code.
    // So we put it here.
    *ENV_LOGGER;
    let mode = *RECORD_MODE;

    Receiver{buffer: RefCell::new(HashMap::new()),
             receiver: Flavor::Never(crossbeam_channel::never()),
             mode }
}
