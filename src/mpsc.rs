use std::sync::mpsc;
// use std::sync::mpsc::{RecvError, SendError, RecvTimeoutError, TryRecvError};
use crate::get_generic_name;
use crate::log_rr;
use crate::rr::mark_program_as_desynced;
use crate::rr::{
    self, get_forward_id, ChannelLabel, DesyncError, DetChannelId, RecordMetadata, RecordedEvent,
    RecvErrorRR, RecvRR, SendRR,
};
use crate::thread::get_and_inc_channel_id;
use crate::thread::*;
use crate::{DesyncMode, RRMode, DESYNC_MODE, ENV_LOGGER, RECORD_MODE};
use log::Level::*;
use std::cell::RefCell;
use std::cell::RefMut;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::error::Error;
use std::time::Duration;
use std::time::Instant;

#[derive(Debug)]
pub struct Sender<T> {
    pub(crate) sender: SenderFlavor<T>,
    pub(crate) metadata: RecordMetadata,
}

#[derive(Debug)]
pub enum ReceiverFlavor<T> {
    Bounded(mpsc::Receiver<(Option<DetThreadId>, T)>),
    Unbounded(mpsc::Receiver<(Option<DetThreadId>, T)>),
}

#[derive(Debug, Clone)]
pub enum SenderFlavor<T> {
    Bounded(mpsc::SyncSender<(Option<DetThreadId>, T)>),
    Unbounded(mpsc::Sender<(Option<DetThreadId>, T)>),
}

/// General template for implementing trait RecvRR for our mpsc::Receiver<T>.
macro_rules! impl_RR {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> RecvRR<T, $err_type> for Receiver<T> {
            fn to_recorded_event(
                &self,
                event: Result<(Option<DetThreadId>, T), $err_type>,
            ) -> (Result<T, $err_type>, RecordedEvent) {
                match event {
                    Ok((sender_thread, msg)) => (Ok(msg), RecordedEvent::$succ { sender_thread }),
                    Err(e) => (Err(e), RecordedEvent::$err(e)),
                }
            }

            fn expected_recorded_events(
                &self,
                event: RecordedEvent,
            ) -> Result<Result<T, $err_type>, DesyncError> {
                match event {
                    RecordedEvent::$succ { sender_thread } => {
                        let retval = self.replay_recv(&sender_thread)?;
                        // Here is where we explictly increment our event_id!
                        inc_event_id();
                        Ok(Ok(retval))
                    }
                    RecordedEvent::$err(e) => {
                        log_rr!(
                            Trace,
                            "Creating error event for: {:?}",
                            RecordedEvent::$err(e)
                        );
                        // Here is where we explictly increment our event_id!
                        inc_event_id();
                        Ok(Err(e))
                    }
                    e => {
                        let mock_event = RecordedEvent::$succ {
                            sender_thread: None,
                        };
                        Err(DesyncError::EventMismatch(e, mock_event))
                    }
                }
            }

            fn get_buffer(&self) -> RefMut<HashMap<Option<DetThreadId>, VecDeque<T>>> {
                self.buffer.borrow_mut()
            }
        }
    };
}

impl_RR!(mpsc::RecvError, MpscRecvSucc, MpscRecvErr);
impl_RR!(mpsc::TryRecvError, MpscTryRecvSucc, MpscTryRecvErr);
impl_RR!(
    mpsc::RecvTimeoutError,
    MpscRecvTimeoutSucc,
    MpscRecvTimeoutErr
);

#[derive(Debug)]
pub struct Receiver<T> {
    pub(crate) receiver: ReceiverFlavor<T>,
    pub(crate) metadata: RecordMetadata,
    pub(crate) buffer: RefCell<HashMap<Option<DetThreadId>, VecDeque<T>>>,
}

impl<T> Receiver<T> {
    pub fn new(real_receiver: ReceiverFlavor<T>, id: DetChannelId) -> Receiver<T> {
        let flavor = Receiver::get_marker(&real_receiver);
        Receiver {
            buffer: RefCell::new(HashMap::new()),
            receiver: real_receiver,
            metadata: RecordMetadata {
                type_name: get_generic_name::<T>().to_string(),
                flavor,
                mode: *RECORD_MODE,
                id,
            },
        }
    }

    pub(crate) fn replay_recv(&self, sender: &Option<DetThreadId>) -> Result<T, DesyncError> {
        let timeout = Duration::from_secs(1);
        let rr_recv_timeout = || {
            self.receiver.recv_timeout(timeout).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => RecvErrorRR::Timeout,
                mpsc::RecvTimeoutError::Disconnected => RecvErrorRR::Disconnected,
            })
        };

        rr::recv_from_sender(
            &sender,
            rr_recv_timeout,
            &mut self.buffer.borrow_mut(),
            &self.metadata.id,
        )
    }

    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        self.rr_recv(self.metadata(), || self.receiver.recv(), "channel::recv()")
            .unwrap_or_else(|e| self.handle_desync(e, true, || self.receiver.recv()))
    }

    pub fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        let f = || self.receiver.try_recv();
        self.rr_recv(self.metadata(), f, "channel::try_recv()")
            .unwrap_or_else(|e| self.handle_desync(e, false, f))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        let f = || self.receiver.recv_timeout(timeout);
        self.rr_recv(self.metadata(), f, "channel::recv_timeout()")
            .unwrap_or_else(|e| self.handle_desync(e, false, f))
    }

    pub fn metadata(&self) -> &RecordMetadata {
        &self.metadata
    }

    pub fn get_marker(receiver: &ReceiverFlavor<T>) -> ChannelLabel {
        match receiver {
            ReceiverFlavor::Unbounded(_) => ChannelLabel::MpscUnbounded,
            ReceiverFlavor::Bounded(_) => ChannelLabel::MpscBounded,
        }
    }
}

/// Helper macro to generate methods for Flavor. Calls appropriate channel method.
macro_rules! generate_receiver_method {
    ($method:ident, $error_type:ty) => {
        pub fn $method(&self) -> Result<(Option<DetThreadId>, T), $error_type> {
            match self {
                ReceiverFlavor::Bounded(receiver) | ReceiverFlavor::Unbounded(receiver) => {
                    receiver.$method()
                }
            }
        }
    };
}

impl<T> ReceiverFlavor<T> {
    generate_receiver_method!(recv, mpsc::RecvError);
    generate_receiver_method!(try_recv, mpsc::TryRecvError);

    pub fn recv_timeout(
        &self,
        duration: Duration,
    ) -> Result<(Option<DetThreadId>, T), mpsc::RecvTimeoutError> {
        // TODO(edumenyo)
        match self {
            ReceiverFlavor::Bounded(receiver) | ReceiverFlavor::Unbounded(receiver) => {
                match receiver.recv_timeout(duration) {
                    Ok(msg) => Ok(msg),
                    e => e,
                }
            }
        }
    }
}

// TODO(edumenyo)
impl<T> Sender<T> {
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, msg: T) -> Result<(), mpsc::SendError<T>> {
        match self.rr_send(
            msg,
            &self.metadata.mode,
            &self.metadata.id,
            &self.metadata.type_name,
            &self.metadata.flavor,
            "Sender",
        ) {
            Ok(v) => v,
            // send() should never hang. No need to check if NoEntryLog.
            Err((error, msg)) => {
                log_rr!(Warn, "Desynchronization detected: {:?}", error);
                match *DESYNC_MODE {
                    DesyncMode::Panic => panic!("Send::Desynchronization detected: {:?}", error),
                    // TODO: One day we may want to record this alternate execution.
                    DesyncMode::KeepGoing => {
                        mark_program_as_desynced();
                        let res = SendRR::send(self, get_forward_id(), msg);
                        // TODO Ugh, right now we have to carefully increase the event_id
                        // in the "right places" or nothing will work correctly.
                        // How can we make this a lot less error prone?
                        inc_event_id();
                        res
                    }
                }
            }
        }
    }
}

impl<T> Clone for Sender<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        // following implementation for crossbeam, we do not support MPSC for bounded channels as the blocking semantics are
        // more complicated to implement.
        if self.metadata.flavor == ChannelLabel::MpscBounded {
            log_rr!(
                Warn,
                "MPSC for bounded channels not supported. Blocking semantics \
                     of bounded channels will not be preseved!"
            );
        }
        Sender {
            sender: self.sender.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

impl<T> SendRR<T, mpsc::SendError<T>> for Sender<T> {
    fn check_log_entry(&self, entry: RecordedEvent) -> Result<(), DesyncError> {
        match entry {
            RecordedEvent::Sender => Ok(()),
            log_event => Err(DesyncError::EventMismatch(log_event, RecordedEvent::Sender)),
        }
    }

    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), mpsc::SendError<T>> {
        self.sender.send((thread_id, msg))
    }

    const EVENT_VARIANT: RecordedEvent = RecordedEvent::MpscSender;
}

macro_rules! generate_try_send {
    () => {
        // if let SenderFlavor::Bounded(sender) = self {
        //     pub fn try_send(&self, t: (Option<DetThreadId>, T)) -> Result<(), mpsc::TrySendError<T>> {
        //         sender.try_send(t);
        //     }
        // }
    };
}

impl<T> SenderFlavor<T> {
    generate_try_send!();

    // TODO(edumenyo)
    pub fn send(&self, t: (Option<DetThreadId>, T)) -> Result<(), mpsc::SendError<T>> {
        // unimplemented!();

        match self {
            Self::Bounded(sender) => sender.send(t).map_err(|e| {
                let msg: (Option<DetThreadId>, T) = e.0;
                mpsc::SendError(msg.1)
            }),
            Self::Unbounded(sender) => sender.send(t).map_err(|e| {
                let msg: (Option<DetThreadId>, T) = e.0;
                mpsc::SendError(msg.1)
            }),
        }
    }
}

// impl<T> Clone for SenderFlavor<T> {
//     fn clone(&self) -> Self {
//         // following implementation for crossbeam, we do not support MPSC for bounded channels as the blocking semantics are
//         // more complicated to implement.
//         if self.metadata.flavor == ChannelLabel::MpscBounded {
//             log_rr!(Warn,
//                     "MPSC for bounded channels not supported. Blocking semantics \
//                      of bounded channels will not be preseved!");
//         }
//         Sender {
//             sender: self.sender.clone(),
//             metadata: self.metadata.clone(),
//         }
//     }
// }

pub fn sync_channel<T>(bound: usize) -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = mpsc::sync_channel(bound);
    let mode = *RECORD_MODE;
    let channel_type = ChannelLabel::MpscBounded;
    let type_name = get_generic_name::<T>();
    let id = DetChannelId::new();

    log_rr!(
        Info,
        "Bounded mpsc channel created: {:?} {:?}",
        id,
        type_name
    );
    let metadata = RecordMetadata {
        type_name: type_name.to_string(),
        flavor: channel_type,
        mode,
        id: id.clone(),
    };
    (
        Sender {
            sender: SenderFlavor::Bounded(sender),
            metadata,
        },
        Receiver::new(ReceiverFlavor::Bounded(receiver), id),
    )
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = mpsc::channel();
    let mode = *RECORD_MODE;
    let channel_type = ChannelLabel::MpscUnbounded;
    let type_name = get_generic_name::<T>();
    let id = DetChannelId::new();

    log_rr!(
        Info,
        "Unbounded mpsc channel created: {:?} {:?}",
        id,
        type_name
    );
    let metadata = RecordMetadata {
        type_name: type_name.to_string(),
        flavor: channel_type,
        mode,
        id: id.clone(),
    };
    (
        Sender {
            sender: SenderFlavor::Unbounded(sender),
            metadata,
        },
        Receiver::new(ReceiverFlavor::Unbounded(receiver), id),
    )
}
