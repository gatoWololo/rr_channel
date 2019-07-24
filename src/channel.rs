use crate::log_trace;
use crate::record_replay::{self, FlavorMarker, Recorded, Blocking};
use crate::thread::get_and_inc_channel_id;
use crate::thread::*;
use crate::log_trace_with;
use crate::RecordReplayMode;
use crate::ENV_LOGGER;
use crate::RECORD_MODE;
use crossbeam_channel;
use crossbeam_channel::RecvError;
use crossbeam_channel::SendError;
use crossbeam_channel::{RecvTimeoutError, TryRecvError};
use log::{debug, trace, warn, info};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::error::Error;
use std::time::Duration;
use std::time::Instant;

use std::cell::RefCell;
use crate::record_replay::DetChannelId;

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let receiver = Flavor::Unbounded(receiver);
    let mode = *RECORD_MODE;
    let channel_type = FlavorMarker::Unbounded;

    let type_name = unsafe { std::intrinsics::type_name::<T>() };
    let id = DetChannelId {
        det_thread_id: get_det_id(),
        channel_id: get_and_inc_channel_id(),
    };

    log_trace(&format!("Unbounded channel created: {:?} {:?}", id, type_name));
    (
        Sender {
            sender,
            mode,
            channel_type,
            channel_id: id.clone(),
            type_name: type_name.to_string()
        },
        Receiver::new(receiver, id),
    )
}

pub struct Sender<T> {
    pub(crate) sender: crossbeam_channel::Sender<(Option<DetThreadId>, T)>,
    // Unlike Receiver, whose channels may have different types, the Sender always
    // has the same channel type. So we just keep a marker around.
    pub(crate) channel_type: FlavorMarker,
    mode: RecordReplayMode,
    /// Unique identifier assigned to every channel. Deterministic and unique
    /// even with racing thread creation. DetThreadId refers to the original
    /// creator of this thread.
    /// The partner Receiver and Sender shares the same id.
    pub(crate) channel_id: DetChannelId,
    pub(crate) type_name: String,
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
            channel_id: self.channel_id.clone(),
            type_name: self.type_name.clone()
        }
    }
}

impl<T> Sender<T> {
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        let det_id = get_det_id();
        log_trace(&format!("Sender<{:?}>::send(({:?}, {:?}))",
                           self.channel_id, det_id, self.type_name));
        // Include send events as increasing the event id for more granular
        // logical times.
        inc_event_id();
        // We send the det_id even when running in RecordReplayMode::NoRR,
        // but that's okay. It makes logic a little simpler.
        // crossbeam::send() returns Result<(), SendError<(DetThreadId, T)>>,
        // we want to make this opaque to the user. Just return the T on error.
        self.sender
            .send((det_id, msg))
            .map_err(|e| SendError(e.into_inner().1))
    }

    /// Similar to send, but uses the passed `id` instead of calling `get_det_id()`.
    /// Useful for the IPC router to "route" the original DetThreadId instead of
    /// the routers.
    /// WARNING: Missuing this function can result in nondeterministic behavior!
    pub fn send_with_id(&self, msg: T, id: Option<DetThreadId>)
                               -> Result<(), SendError<T>> {
        log_trace(&format!("Sender<{:?}>::send_with_id()", self.channel_id));
        log_trace(&format!("Forwarding ID: {:?}", id));
        self.sender
            .send((id, msg))
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
    Bounded(crossbeam_channel::Receiver<(Option<DetThreadId>, T)>),
    Never(crossbeam_channel::Receiver<(Option<DetThreadId>, T)>),
    Unbounded(crossbeam_channel::Receiver<(Option<DetThreadId>, T)>),
}

/// Helper macro to generate methods for Flavor. Calls appropriate channel method.
/// On case of Flavor::After inserts get_det_id() as the sender_thread_id in order
/// to have the correct type.
macro_rules! generate_channel_method {
    ($method:ident, $error_type:ty) => {
        pub fn $method(&self) -> Result<(Option<DetThreadId>, T), $error_type> {
            // Not really useful.
            // log_trace(concat!("Flavor::", stringify!($method)));
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

    pub fn recv_timeout(&self, duration: Duration) -> Result<(Option<DetThreadId>, T), RecvTimeoutError> {
        // Not really useful.
        // log_trace("Flavor::recv_timeout");
        match self {
            Flavor::After(receiver) => match receiver.recv_timeout(duration) {
                Ok(msg) => Ok((get_det_id(), msg)),
                e => e.map(|_| unreachable!()),
            },
            Flavor::Bounded(receiver) | Flavor::Never(receiver) | Flavor::Unbounded(receiver) => {
                receiver.recv_timeout(duration)
            }
        }
    }
}

pub struct Receiver<T> {
    /// Buffer holding values from the "wrong" thread on replay mode.
    /// Crossbeam works with inmutable references, so we wrap in a RefCell
    /// to hide our mutation.
    buffer: RefCell<HashMap<DetThreadId, VecDeque<T>>>,
    /// Separate buffer to hold entries whose DetThreadId was None.
    /// TODO this might be nondeterministic if different threads are sending
    /// these requests.
    /// Even tracking their TID would be useful to check for multiple producers
    /// "none" senders.
    none_buffer: RefCell<VecDeque<T>>,
    pub(crate) receiver: Flavor<T>,
    pub(crate) metadata: RecordMetadata,
}

macro_rules! impl_RecordReplay {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> RecordReplay<T, $err_type> for Receiver<T> {
            fn to_recorded_event(
                &self,
                event: Result<(Option<DetThreadId>, T), $err_type>,
            ) -> (Result<T, $err_type>, Recorded) {
                match event {
                    Ok((sender_thread, msg)) => (Ok(msg), Recorded::$succ { sender_thread }),
                    Err(e) => (Err(e), Recorded::$err(e)),
                }
            }

            fn expected_recorded_events(&self, event: &Recorded) -> Result<T, $err_type> {
                match event {
                    Recorded::$succ { sender_thread } => {
                        let sender_thread = sender_thread.clone();
                        Ok(self.replay_recv(&sender_thread))
                    }

                    Recorded::$err(e) => {
                        log_trace(&format!("Creating error event for: {:?}", Recorded::$err(*e)));
                        Err(*e)
                    },
                    e => {
                        let error = format!("Unexpected event: {:?} in replay for channel::recv()", e);
                        log_trace(&error);
                        panic!(error);
                    }
                }
            }

            fn replay_recv(&self, sender: &Option<DetThreadId>) -> T {
                record_replay::replay_recv(&sender,
                                           || self.receiver.recv(),
                                           &mut self.buffer.borrow_mut(),
                                           &mut self.none_buffer.borrow_mut(),
                                           &self.metadata.id,
                )
            }
        }
    };
}

use crate::record_replay::RecordMetadata;
use crate::record_replay::RecordReplay;

impl_RecordReplay!(RecvError, RecvSucc, RecvErr);
impl_RecordReplay!(TryRecvError, TryRecvSucc, TryRecvErr);
impl_RecordReplay!(RecvTimeoutError, RecvTimeoutSucc, RecvTimeoutErr);

impl<T> Receiver<T> {
    pub fn new(real_receiver: Flavor<T>, id: DetChannelId) -> Receiver<T> {
        let flavor = Receiver::get_marker(&real_receiver);
        Receiver {
            buffer: RefCell::new(HashMap::new()),
            none_buffer: RefCell::new(VecDeque::new()),
            receiver: real_receiver,
            metadata: RecordMetadata {
                type_name: unsafe { std::intrinsics::type_name::<T>().to_string() },
                flavor,
                mode: *RECORD_MODE,
                id
            },
        }
    }

    pub fn flavor(&self) -> FlavorMarker {
        self.metadata.flavor
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        self.record_replay(self.metadata(), || self.receiver.recv(),
                           Blocking::CanBlock, "channel::recv()")
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.record_replay(self.metadata(), || self.receiver.try_recv(),
                           Blocking::CannotBlock, "channel::try_recv()")
    }

    // TODO: Hmmm, even though we label this function as CannotBlock.
    // We might not have an event if the program ended right as we were waiting
    // for recv_timeout() but before the timeout happended.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        self.record_replay(self.metadata(), || self.receiver.recv_timeout(timeout),
                           Blocking::CannotBlock, "channel::rect_timeout()")
    }

    pub fn metadata(&self) -> &RecordMetadata {
        &self.metadata
    }

    // TODO: Move borrow_mut() outside the loop.
    pub fn replay_recv(&self, sender: &Option<DetThreadId>) -> T {
        record_replay::replay_recv(
            sender,
            || self.receiver.recv(),
            &mut self.buffer.borrow_mut(),
            &mut self.none_buffer.borrow_mut(),
            &self.metadata.id,
        )
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
    let id = DetChannelId {
            det_thread_id: get_det_id(),
            channel_id: get_and_inc_channel_id()
    };
    log_trace(&format!("After channel receiver created: {:?}", id));
    Receiver::new(Flavor::After(crossbeam_channel::after(duration)), id)
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
    let id = DetChannelId {
        det_thread_id: get_det_id(),
        channel_id: get_and_inc_channel_id(),
    };

    let type_name = unsafe { std::intrinsics::type_name::<T>() };
    log_trace(&format!("Bounded channel created: {:?} {:?}", id, type_name));
    (
        Sender {
            sender,
            mode,
            channel_type,
            channel_id: id.clone(),
            type_name: type_name.to_string()
        },
        Receiver::new(receiver, id),
    )
}

pub fn never<T>() -> Receiver<T> {
    *ENV_LOGGER;
    let type_name = unsafe { std::intrinsics::type_name::<T>().to_string() };
    let id = DetChannelId {
        det_thread_id: get_det_id(),
        channel_id: get_and_inc_channel_id(),
    };

    Receiver {
        buffer: RefCell::new(HashMap::new()),
        receiver: Flavor::Never(crossbeam_channel::never()),
        none_buffer: RefCell::new(VecDeque::new()),
        metadata: RecordMetadata {
            type_name,
            flavor: FlavorMarker::Never,
            mode: *RECORD_MODE,
            id,
        },
    }
}

pub(crate) fn get_log_event() -> (Recorded, FlavorMarker) {
    let det_id = get_det_id();
    let event_id = get_event_id();

    let (event, flavor) =
        record_replay::get_log_entry(det_id.expect("get_log_event()"), event_id).
        expect("get_log_event(): No such key in map.");
    inc_event_id();

    (event.clone(), *flavor)
}
