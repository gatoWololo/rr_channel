use crate::log_trace;
use crate::record_replay::{
    self, FlavorMarker, Recorded,
};
use crate::thread::get_and_inc_channel_id;
use crate::thread::*;
use crate::RecordReplayMode;
use crate::ENV_LOGGER;
use crate::RECORD_MODE;
use crossbeam_channel;
use crossbeam_channel::RecvError;
use crossbeam_channel::SendError;
use crossbeam_channel::{RecvTimeoutError, TryRecvError};
use log::{debug, trace, warn};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;
use std::error::Error;

use std::cell::RefCell;

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let receiver = Flavor::Unbounded(receiver);
    let mode = *RECORD_MODE;
    let channel_type = FlavorMarker::Unbounded;

    let type_name = unsafe { std::intrinsics::type_name::<T>() };
    let id = (get_det_id(), get_and_inc_channel_id());
    (
        Sender {
            sender,
            mode,
            channel_type,
            id: id.clone(),
        },
        Receiver::new(receiver, id)
    )
}

pub struct Sender<T> {
    pub(crate) sender: crossbeam_channel::Sender<(DetThreadId, T)>,
    // Unlike Receiver, whose channels may have different types, the Sender always
    // has the same channel type. So we just keep a marker around.
    pub(crate) channel_type: FlavorMarker,
    mode: RecordReplayMode,
    /// Unique identifier assigned to every channel. Deterministic and unique
    /// even with racing thread creation. DetThreadId refers to the original
    /// creator of this thread.
    /// The partner Receiver and Sender shares the same id.
    pub(crate) id: (DetThreadId, u32),
}

/// crossbeam_channel::Sender does not derive clone. Instead it implements it,
/// this is to avoid the constraint that T must be Clone.
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        // We do not support MPSC for bounded channels as the blocking semantics are
        // more complicated to implement.
        if self.channel_type == FlavorMarker::Bounded {
            warn!(
                "MPSC for bounded channels not supported. Blocking semantics \
                 of bounded channels will not be preseved!"
            );
        }
        Sender {
            sender: self.sender.clone(),
            mode: self.mode.clone(),
            channel_type: self.channel_type,
            id: self.id.clone(),
        }
    }
}

impl<T> Sender<T> {
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        log_trace(&format!("Sender<{:?}>::send()", self.id));
        // We send the det_id even when running in RecordReplayMode::NoRR,
        // but that's okay. It makes logic a little simpler.
        // crossbeam::send() returns Result<(), SendError<(DetThreadId, T)>>,
        // we want to make this opaque to the user. Just return the T on error.
        self.sender
            .send((get_det_id(), msg))
            .map_err(|e| SendError(e.into_inner().1))
    }
}

// use std::fmt::Debug;
// trait PrintData<T> {
//     fn get_debug(&self, data: T) -> String;
// }

// impl<T> PrintData<T> for Sender<T> {
//     default fn get_debug(&self, data: T) -> String {
//         "No Debug Data".to_string()
//     }
// }

// impl<T: Debug> PrintData<T> for Sender<T> {
//     fn get_debug(&self, data: T) -> String {
//         format!("{:?}", data)
//     }
// }

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
            log_trace(concat!("Flavor::", stringify!($method)));
            match self {
                Flavor::After(receiver) => {
                    match receiver.$method() {
                        Ok(msg) => Ok((get_det_id(), msg)),
                        e => e.map(|_| unreachable!()),
                    }
                }
                Flavor::Bounded(receiver) |
                Flavor::Unbounded(receiver) |
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
            log_trace("Flavor::recv_timeout");
            match self {
                Flavor::After(receiver) => match receiver.recv_timeout(duration) {
                    Ok(msg) => Ok((get_det_id(), msg)),
                    e => e.map(|_| unreachable!()),
                },
                Flavor::Bounded(receiver) |
                Flavor::Never(receiver) |
                Flavor::Unbounded(receiver) => {
                    receiver.recv_timeout(duration)
                },
            }
        }
}

pub struct Receiver<T> {
    /// Buffer holding values from the "wrong" thread on replay mode.
    /// Crossbeam works with inmutable references, so we wrap in a RefCell
    /// to hide our mutation.
    buffer: RefCell<HashMap<DetThreadId, VecDeque<T>>>,
    pub(crate) receiver: Flavor<T>,
    metadata: RecordMetadata,
}

macro_rules! impl_RecordReplay {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> RecordReplay<T, $err_type> for Receiver<T> {
            fn to_recorded_event(&self, event: Result<(DetThreadId, T), $err_type>) ->
                (Result<T, $err_type>, Recorded) {
                    match event {
                        Ok((sender_thread, msg)) =>
                            (Ok(msg), Recorded::$succ { sender_thread }),
                        Err(e) =>
                            (Err(e), Recorded::$err(e)),
                    }
                }

            fn expected_recorded_events(&self, event: Recorded) -> Result<T, $err_type> {
                match event {
                    Recorded::$succ {sender_thread} =>
                        Ok(self.replay_recv(&sender_thread)),
                    Recorded::$err(e) => Err(e),
                    _ => panic!("Unexpected event: {:?} in replay for recv()",)
                }

            }

            fn replay_recv(&self, sender: &DetThreadId) -> T {
                record_replay::replay_recv(&sender, self.buffer.borrow_mut(),
                || self.receiver.recv())
            }
        }
    }
}

use crate::record_replay::RecordMetadata;
use crate::record_replay::RecordReplay;

impl_RecordReplay!(RecvError, RecvSucc, RecvErr);
impl_RecordReplay!(TryRecvError, TryRecvSucc, TryRecvErr);
impl_RecordReplay!(RecvTimeoutError, RecvTimeoutSucc, RecvTimeoutErr);

impl<T> Receiver<T> {
    pub fn new(real_receiver: Flavor<T>, id: (DetThreadId, u32)) -> Receiver<T> {
        let flavor = Receiver::get_marker(&real_receiver);
        Receiver {
            buffer: RefCell::new(HashMap::new()),
            receiver: real_receiver,
            metadata: RecordMetadata {
                type_name: unsafe { std::intrinsics::type_name::<T>() },
                flavor,
                mode: *RECORD_MODE,
                id: (get_det_id(), get_and_inc_channel_id())
            }
        }
    }

    pub fn flavor(&self) -> FlavorMarker {
        self.metadata.flavor
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        self.record_replay(self.metadata(), || self.receiver.recv())
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.record_replay(self.metadata(), || self.receiver.try_recv())
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        self.record_replay(self.metadata(), || self.receiver.recv_timeout(timeout))
    }

    pub fn metadata(&self) -> &RecordMetadata {
        &self.metadata
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
                },
                Err(e) => {
                    debug!("Saw Err({:?})", e);
                    // Got a disconnected message. Ignore.
                },
            }
        }
    }

    pub fn get_marker(receiver: &Flavor<T>) -> FlavorMarker {
        match receiver {
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

    Receiver::new(Flavor::After(crossbeam_channel::after(duration)),
                  (get_det_id(), get_and_inc_channel_id()))
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
    let type_name = unsafe { std::intrinsics::type_name::<T>() };
    let id = (get_det_id(), get_and_inc_channel_id());
    (
        Sender {
            sender,
            mode,
            channel_type,
            id: id.clone(),
        },
        Receiver::new(receiver, id)
    )
}

pub fn never<T>() -> Receiver<T> {
    *ENV_LOGGER;

    Receiver {
        buffer: RefCell::new(HashMap::new()),
        receiver: Flavor::Never(crossbeam_channel::never()),
        metadata: RecordMetadata {
            type_name: unsafe { std::intrinsics::type_name::<T>() },
            flavor: FlavorMarker::Never,
            mode: *RECORD_MODE,
            id: (get_det_id(), get_and_inc_channel_id())
        }
    }
}

pub(crate) fn get_log_event() -> (Recorded, FlavorMarker) {
    let det_id = get_det_id();
    let select_id = get_select_id();

    let (event, flavor) =
        record_replay::get_log_entry(det_id, select_id).
        expect("No such key in map.");
    inc_select_id();

    (event.clone(), *flavor)}

