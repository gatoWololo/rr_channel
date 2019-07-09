use crossbeam_channel;
use crate::RecordReplayMode;
use log::{trace, debug, warn};
use crossbeam_channel::SendError;
use crate::thread::*;
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
    let channel_type = FlavorMarker::Unbounded;

    let type_name = unsafe {std::intrinsics::type_name::<T>()};
    // println!("Type name: {}", type_name);
    (Sender {sender, mode, channel_type },
     Receiver{buffer: RefCell::new(HashMap::new()), receiver, mode, type_name })
}


pub struct Sender<T>{
    pub(crate) sender: crossbeam_channel::Sender<(DetThreadId, T)>,
    // Unlike Receiver, whose channels may have different types, the Sender always
    // has the same channel type. So we just keep a marker around.
    pub(crate) channel_type: FlavorMarker,
    mode: RecordReplayMode,
}

/// crossbeam_channel::Sender does not derive clone. Instead it implements it,
/// this is to avoid the constraint that T must be Clone.
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        // We do not support MPSC for bounded channels as the blocking semantics are
        // more complicated to implement.
        if self.channel_type == FlavorMarker::Bounded {
            warn!("MPSC for bounded channels not supported. Blocking semantics \
                   of bounded channels will not be preseved!");
        }
        Sender { sender: self.sender.clone(), mode: self.mode.clone(),
                 channel_type: self.channel_type }
    }
}


impl<T> Sender<T> {
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        // We send the det_id even when running in RecordReplayMode::NoRR,
        // but that's okay. It makes logic a little simpler.
        let det_id = get_det_id();
        trace!("Sender::send()");

        // crossbeam::send() returns Result<(), SendError<(DetThreadId, T)>>,
        // we want to make this opaque to the user. Just return the T on error.
        self.sender.send((det_id, msg)).
            map_err(|e| SendError(e.into_inner().1))
    }
}

use std::fmt::Debug;
trait PrintData<T> {
    fn get_debug(&self, data: T) -> String;
}

impl<T> PrintData<T> for Sender<T> {
    default fn get_debug(&self, data: T) -> String {
        "No Debug Data".to_string()
    }
}

impl<T: Debug> PrintData<T> for Sender<T> {
    fn get_debug(&self, data: T) -> String {
        format!("{:?}", data)
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
                Flavor::Bounded(receiver) |
                Flavor::Never(receiver)   |
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
    type_name: &'static str
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, RecvError> {
        trace!("Receiver::recv(), channel:");
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
                record_replay::log(RecordedEvent::Receive(event), self.flavor(), self.type_name);
                result
            }

            RecordReplayMode::Replay => {
                let det_id = get_det_id();
                let select_id = get_select_id();
                trace!("Replaying Receiver::recv() event.");
                let (event, flavor) =
                    record_replay::get_log_entry(det_id, select_id).
                    expect("No such key in map.");
                inc_select_id();

                if *flavor != self.flavor() {
                    panic!("Expected {:?}, saw {:?}", flavor, self.flavor());
                }

                match event {
                    RecordedEvent::Receive(
                        ReceiveEvent::Success{sender_thread}) => {
                        Ok(self.replay_recv(&sender_thread))
                    }
                    RecordedEvent::Receive(ReceiveEvent::RecvError) => {
                        if *flavor != self.flavor() {
                            panic!("Expected {:?}, saw {:?}", flavor, self.flavor());
                        }
                        trace!("Saw RecvError on record, creating artificial one.");
                        return Err(RecvError);
                    }
                    _ => panic!("Unexpected event: {:?} in replay for recv()"),
                }
            }
            RecordReplayMode::NoRR => {
                match self.receiver.recv() {
                    // Err(e) on the RHS is not the same type as Err(e) LHS.
                    Err(e) => Err(e),
                    Ok((_, msg)) => Ok(msg),
                }
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
        trace!("Receiver::try_recv()");
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
                record_replay::log(RecordedEvent::TryRecv(event), self.flavor(), self.type_name);
                result
            }
            RecordReplayMode::Replay => {
                let pair = (get_det_id(), get_select_id());
                let (event, flavor) =
                    record_replay::get_log_entry(get_det_id(), get_select_id()).
                    expect("No such key in log");
                inc_select_id();

                if *flavor != self.flavor() {
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
                    e => panic!("Unexpected event {:?}: {:?} in replay for try_recv()", pair, e),
                }
            }
            RecordReplayMode::NoRR => {
                match self.receiver.try_recv() {
                    Err(e) => Err(e),
                    Ok((_, msg)) => Ok(msg),
                }
            }
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        trace!("Receiver::recv_timeout()");
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
                record_replay::log(RecordedEvent::RecvTimeout(event), self.flavor(), self.type_name);
                result
            }
            RecordReplayMode::Replay => {
                let (event, flavor) =
                    record_replay::get_log_entry(get_det_id(), get_select_id()).
                    expect("No such key in map.");
                inc_select_id();

                if *flavor != self.flavor() {
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
                    e => panic!("Unexpected event: {:?} in replay for recv_timeout()", e),
                }
            }
            RecordReplayMode::NoRR => {
                match self.receiver.recv_timeout(timeout) {
                    Err(e) => Err(e),
                    Ok((_, msg)) => Ok(msg)
                }

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
             mode, type_name: "after" }
}

pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    // Init log, happens once, lazily.
    // Users always have to make channels before using the rest of this code.
    // So we put it here.
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::bounded(cap);
    let receiver = Flavor::Bounded(receiver);
    let mode = *RECORD_MODE;
    let channel_type = FlavorMarker::Bounded;
    let type_name = unsafe {std::intrinsics::type_name::<T>()};

    (Sender {sender, mode, channel_type },
     Receiver{buffer: RefCell::new(HashMap::new()), receiver, mode, type_name })
}

pub fn never<T>() -> Receiver<T> {
    // Init log, happens once, lazily.
    // Users always have to make channels before using the rest of this code.
    // So we put it here.
    *ENV_LOGGER;
    let mode = *RECORD_MODE;
    let type_name = unsafe {std::intrinsics::type_name::<T>()};

    Receiver{buffer: RefCell::new(HashMap::new()),
             receiver: Flavor::Never(crossbeam_channel::never()),
             mode, type_name }
}
