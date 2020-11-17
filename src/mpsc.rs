use log::Level::*;
use std::cell::RefCell;
use std::cell::RefMut;
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use crate::{desync, EventRecorder};
use crate::detthread::{self, DetThreadId};
use crate::error::{DesyncError, RecvErrorRR};
use crate::recordlog::{self, ChannelLabel, RecordedEvent};
use crate::rr::RecvRecordReplay;
use crate::rr::SendRecordReplay;
use crate::rr::{self, DetChannelId};
use crate::{DesyncMode, DetMessage};
use crate::{get_generic_name, DESYNC_MODE, ENV_LOGGER, RECORD_MODE, BufferedValues};

#[derive(Debug)]
pub struct Sender<T> {
    pub(crate) sender: RealSender<T>,
    pub(crate) metadata: recordlog::RecordMetadata,
    event_recorder: EventRecorder,
}

#[derive(Debug)]
pub enum RealReceiver<T> {
    Bounded(mpsc::Receiver<DetMessage<T>>),
    Unbounded(mpsc::Receiver<DetMessage<T>>),
}

#[derive(Debug, Clone)]
pub enum RealSender<T> {
    Bounded(mpsc::SyncSender<DetMessage<T>>),
    Unbounded(mpsc::Sender<DetMessage<T>>),
}

/// General template for implementing trait RecvRR for our mpsc::Receiver<T>.
macro_rules! impl_RR {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> rr::RecvRecordReplay<T, $err_type> for Receiver<T> {

            fn recorded_event_succ(dtid: DetThreadId) -> recordlog::RecordedEvent {
                RecordedEvent::$succ { sender_thread: dtid }
            }

            fn recorded_event_err(e: &$err_type) -> recordlog::RecordedEvent {
                RecordedEvent::$err(*e)
            }

            fn replay_recorded_event(
                &self,
                event: RecordedEvent,
            ) -> desync::Result<Result<T, $err_type>> {
                match event {
                    RecordedEvent::$succ { sender_thread } => {
                        let retval = self.replay_recv(&sender_thread)?;
                        // Here is where we explictly increment our event_id!
                        detthread::inc_event_id();
                        Ok(Ok(retval))
                    }
                    RecordedEvent::$err(e) => {
                        crate::log_rr!(
                            Trace,
                            "Creating error event for: {:?}",
                            RecordedEvent::$err(e)
                        );
                        // Here is where we explictly increment our event_id!
                        detthread::inc_event_id();
                        Ok(Err(e))
                    }
                    e => {
                        let mock_event = RecordedEvent::$succ {
                        // TODO: Is there a better value to show this is a mock DTI?
                        sender_thread: DetThreadId::new(),
                        };
                        Err(DesyncError::EventMismatch(e, mock_event))
                    }
                }
            }
        }
    };
}

impl_RR!(mpsc::RecvError, MpscRecvSucc, MpscRecvErr);
impl_RR!(mpsc::TryRecvError, MpscTryRecvSucc, MpscTryRecvErr);
impl_RR!(mpsc::RecvTimeoutError, MpscRecvTimeoutSucc, MpscRecvTimeoutErr);

#[derive(Debug)]
pub struct Receiver<T> {
    pub(crate) receiver: RealReceiver<T>,
    pub(crate) metadata: recordlog::RecordMetadata,
    /// Sometimes we get the "wrong" message (out of order) when receiving messages. Those messages
    /// get stored here until needed. We always check the buffer first.
    /// Refcell needed as std::sync::mpsc::Receiver API calls for receiver methods take a &self
    /// instead of &mut self (not sure why). So we need internal mutability.
    pub(crate) buffer: RefCell<BufferedValues<T>>,
    event_recorder: EventRecorder
}

impl<T> Receiver<T> {
    fn new(real_receiver: RealReceiver<T>, id: DetChannelId) -> Receiver<T> {
        let flavor = Receiver::get_marker(&real_receiver);
        Receiver {
            buffer: RefCell::new(HashMap::new()),
            receiver: real_receiver,
            metadata: recordlog::RecordMetadata {
                type_name: get_generic_name::<T>().to_string(),
                flavor,
                mode: *RECORD_MODE,
                id,
            },
            event_recorder: EventRecorder::new_file_recorder(),
        }
    }

    fn replay_recv(&self, sender: &DetThreadId) -> desync::Result<T> {
        let timeout = Duration::from_secs(1);
        let rr_recv_timeout = || {
            self.receiver.recv_timeout(timeout).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => RecvErrorRR::Timeout,
                mpsc::RecvTimeoutError::Disconnected => RecvErrorRR::Disconnected,
            })
        };

        rr::recv_expected_message(
            &sender,
            rr_recv_timeout,
            &mut self.get_buffer(),
            // &self.metadata.id,
        )
    }

    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        self.record_replay_with(self.metadata(), || self.receiver.recv(), self.event_recorder.get_recordable())
            .unwrap_or_else(|e| desync::handle_desync(e, || self.receiver.recv(), self.get_buffer()))
    }

    pub fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        let f = || self.receiver.try_recv();
        self.record_replay_with(self.metadata(), f, self.event_recorder.get_recordable())
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        let f = || self.receiver.recv_timeout(timeout);
        self.record_replay_with(self.metadata(), f, self.event_recorder.get_recordable())
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub(crate) fn metadata(&self) -> &recordlog::RecordMetadata {
        &self.metadata
    }

    /// Get label by looking at the type of channel.
    fn get_marker(receiver: &RealReceiver<T>) -> ChannelLabel {
        match receiver {
            RealReceiver::Unbounded(_) => ChannelLabel::MpscUnbounded,
            RealReceiver::Bounded(_) => ChannelLabel::MpscBounded,
        }
    }

    pub(crate) fn get_buffer(&self) -> RefMut<BufferedValues<T>> {
        self.buffer.borrow_mut()
    }
}

/// Helper macro to generate methods for real receiver. Calls appropriate channel method.
macro_rules! generate_receiver_method {
    ($method:ident, $error_type:ty) => {
        pub fn $method(&self) -> Result<DetMessage<T>, $error_type> {
            match self {
                RealReceiver::Bounded(receiver) | RealReceiver::Unbounded(receiver) => {
                    receiver.$method()
                }
            }
        }
    };
}

impl<T> RealReceiver<T> {
    generate_receiver_method!(recv, mpsc::RecvError);
    generate_receiver_method!(try_recv, mpsc::TryRecvError);

    pub fn recv_timeout(
        &self,
        duration: Duration,
    ) -> Result<DetMessage<T>, mpsc::RecvTimeoutError> {
        // TODO(edumenyo)
        match self {
            RealReceiver::Bounded(receiver) | RealReceiver::Unbounded(receiver) => {
                match receiver.recv_timeout(duration) {
                    Ok(msg) => Ok(msg),
                    e => e,
                }
            }
        }
    }
}

impl<T> Sender<T> {
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, msg: T) -> Result<(), mpsc::SendError<T>> {
        match self.record_replay_send(
            msg,
            &self.metadata,
            self.event_recorder.get_recordable(),
        ) {
            Ok(v) => v,
            // send() should never hang. No need to check if NoEntryLog.
            Err((error, msg)) => {
                crate::log_rr!(Warn, "Desynchronization detected: {:?}", error);
                match *DESYNC_MODE {
                    DesyncMode::Panic => panic!("Send::Desynchronization detected: {:?}", error),
                    // TODO: One day we may want to record this alternate execution.
                    DesyncMode::KeepGoing => {
                        desync::mark_program_as_desynced();
                        let res = rr::SendRecordReplay::underlying_send(self, detthread::get_forwarding_id(), msg);
                        // TODO Ugh, right now we have to carefully increase the event_id
                        // in the "right places" or nothing will work correctly.
                        // How can we make this a lot less error prone?
                        detthread::inc_event_id();
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
            crate::log_rr!(
                Warn,
                "MPSC for bounded channels not supported. Blocking semantics \
                     of bounded channels will not be preseved!"
            );
        }
        Sender {
            sender: self.sender.clone(),
            metadata: self.metadata.clone(),
            event_recorder: EventRecorder::new_file_recorder(),
        }
    }
}

impl<T> rr::SendRecordReplay<T, mpsc::SendError<T>> for Sender<T> {
    fn check_log_entry(&self, entry: RecordedEvent) -> desync::Result<()> {
        match entry {
            RecordedEvent::MpscSender => Ok(()),
            log_event => Err(DesyncError::EventMismatch(log_event, RecordedEvent::MpscSender)),
        }
    }

    fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), mpsc::SendError<T>> {
        self.sender.send((thread_id, msg))
    }

    const EVENT_VARIANT: RecordedEvent = RecordedEvent::MpscSender;
}

macro_rules! generate_try_send {
    () => {
        // if let SenderFlavor::Bounded(sender) = self {
        //     pub fn try_send(&self, t: DetMessage<T>) -> Result<(), mpsc::TrySendError<T>> {
        //         sender.try_send(t);
        //     }
        // }
    };
}

impl<T> RealSender<T> {
    generate_try_send!();

    // TODO(edumenyo)
    pub fn send(&self, t: DetMessage<T>) -> Result<(), mpsc::SendError<T>> {
        match self {
            Self::Bounded(sender) => sender.send(t).map_err(|e| {
                let msg: DetMessage<T> = e.0;
                mpsc::SendError(msg.1)
            }),
            Self::Unbounded(sender) => sender.send(t).map_err(|e| {
                let msg: DetMessage<T> = e.0;
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
//             crate::log_rr!(Warn,
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

    crate::log_rr!(
        Info,
        "Bounded mpsc channel created: {:?} {:?}",
        id,
        type_name
    );
    (
        Sender {
            sender: RealSender::Bounded(sender),
            metadata: recordlog::RecordMetadata {
                type_name: type_name.to_string(),
                flavor: channel_type,
                mode,
                id: id.clone(),
            },
            event_recorder: EventRecorder::new_file_recorder(),
        },
        Receiver::new(RealReceiver::Bounded(receiver), id)
    )
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = mpsc::channel();
    let mode = *RECORD_MODE;
    let channel_type = ChannelLabel::MpscUnbounded;
    let type_name = get_generic_name::<T>();
    let id = DetChannelId::new();

    crate::log_rr!(
        Info,
        "Unbounded mpsc channel created: {:?} {:?}",
        id,
        type_name
    );

    (
        Sender {
            sender: RealSender::Unbounded(sender),
            metadata: recordlog::RecordMetadata {
                type_name: type_name.to_string(),
                flavor: channel_type,
                mode,
                id: id.clone(),
            },
            event_recorder: EventRecorder::new_file_recorder(),
        },
        Receiver::new(RealReceiver::Unbounded(receiver), id),
    )
}
