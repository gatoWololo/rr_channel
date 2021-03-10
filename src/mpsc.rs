use log::Level::*;
use std::cell::RefCell;
use std::cell::RefMut;
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use crate::detthread::{self, DetThreadId};
use crate::error::{DesyncError, RecvErrorRR};
use crate::recordlog::{self, ChannelVariant, RecordEntry, RecordMetadata, RecordedEvent};
use crate::rr::RecvRecordReplay;
use crate::rr::SendRecordReplay;
use crate::rr::{self, DetChannelId};
use crate::{desync, get_rr_mode, EventRecorder, RRMode};
use crate::{BufferedValues, DESYNC_MODE};
use crate::{DesyncMode, DetMessage};
use std::any::type_name;

pub use mpsc::{RecvError, RecvTimeoutError, TryRecvError};

#[derive(Debug)]
pub struct Sender<T> {
    pub(crate) sender: RealSender<T>,
    pub(crate) metadata: recordlog::RecordMetadata,
    mode: RRMode,
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
                RecordedEvent::$succ {
                    sender_thread: dtid,
                }
            }

            fn recorded_event_err(e: &$err_type) -> recordlog::RecordedEvent {
                RecordedEvent::$err(*e)
            }

            fn check_event_mismatch(
                record_entry: &RecordEntry,
                metadata: &RecordMetadata,
            ) -> Result<(), DesyncError> {
                match &record_entry.event {
                    // These two are the expected enum variants. That's okay!
                    RecordedEvent::$succ { sender_thread: _ } => {
                        record_entry.check_mismatch(metadata)
                    }
                    RecordedEvent::$err(_) => record_entry.check_mismatch(metadata),
                    // Unexpected enum variant!
                    _ => unreachable!(
                        "We should have already checked for this in {}",
                        stringify!(check_event_mismatch)
                    ),
                }
            }

            fn replay_recorded_event(
                &self,
                event: RecordedEvent,
            ) -> desync::Result<Result<T, $err_type>> {
                match event {
                    RecordedEvent::$succ { sender_thread } => {
                        let retval = self.replay_recv(&sender_thread)?;
                        Ok(Ok(retval))
                    }
                    RecordedEvent::$err(e) => {
                        crate::log_rr!(
                            Trace,
                            "Creating error event for: {:?}",
                            RecordedEvent::$err(e)
                        );
                        Ok(Err(e))
                    }
                    _ => unreachable!(
                        "We should have already checked for this in {}",
                        stringify!(check_event_mismatch)
                    ),
                }
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
    pub(crate) receiver: RealReceiver<T>,
    pub(crate) metadata: recordlog::RecordMetadata,
    /// Sometimes we get the "wrong" message (out of order) when receiving messages. Those messages
    /// get stored here until needed. We always check the buffer first.
    /// Refcell needed as std::sync::mpsc::Receiver API calls for receiver methods take a &self
    /// instead of &mut self (not sure why). So we need internal mutability.
    pub(crate) buffer: RefCell<BufferedValues<T>>,
    event_recorder: EventRecorder,
    mode: RRMode,
}

impl<T> Receiver<T> {
    fn new(real_receiver: RealReceiver<T>, id: DetChannelId) -> Receiver<T> {
        let flavor = Receiver::get_marker(&real_receiver);
        Receiver {
            buffer: RefCell::new(HashMap::new()),
            receiver: real_receiver,
            metadata: recordlog::RecordMetadata {
                type_name: type_name::<T>().to_string(),
                channel_variant: flavor,
                id,
            },
            event_recorder: EventRecorder::get_global_recorder(),
            mode: get_rr_mode(),
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
        let f = || self.receiver.recv();
        self.record_replay(f)
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        let f = || self.receiver.try_recv();
        self.record_replay(f)
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        let f = || self.receiver.recv_timeout(timeout);
        self.record_replay(f)
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    fn record_replay<E>(
        &self,
        g: impl FnOnce() -> Result<DetMessage<T>, E>,
    ) -> desync::Result<Result<T, E>>
    where
        Self: RecvRecordReplay<T, E>,
    {
        self.record_replay_recv(
            &self.mode,
            &self.metadata,
            g,
            self.event_recorder.get_recordable(),
        )
    }

    /// Get label by looking at the type of channel.
    fn get_marker(receiver: &RealReceiver<T>) -> ChannelVariant {
        match receiver {
            RealReceiver::Unbounded(_) => ChannelVariant::MpscUnbounded,
            RealReceiver::Bounded(_) => ChannelVariant::MpscBounded,
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
            &self.mode,
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
                        rr::SendRecordReplay::underlying_send(
                            self,
                            detthread::get_forwarding_id(),
                            msg,
                        )
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
        if self.metadata.channel_variant == ChannelVariant::MpscBounded {
            crate::log_rr!(
                Warn,
                "MPSC for bounded channels not supported. Blocking semantics \
                     of bounded channels will not be preseved!"
            );
        }
        Sender {
            sender: self.sender.clone(),
            metadata: self.metadata.clone(),
            mode: self.mode,
            event_recorder: EventRecorder::get_global_recorder(),
        }
    }
}

impl<T> rr::SendRecordReplay<T, mpsc::SendError<T>> for Sender<T> {
    const EVENT_VARIANT: RecordedEvent = RecordedEvent::MpscSender;

    fn check_log_entry(&self, entry: RecordedEvent) -> desync::Result<()> {
        match entry {
            RecordedEvent::MpscSender => Ok(()),
            log_event => Err(DesyncError::EventMismatch(
                log_event,
                RecordedEvent::MpscSender,
            )),
        }
    }

    fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), mpsc::SendError<T>> {
        self.sender.send((thread_id, msg))
    }
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
    // *ENV_LOGGER;

    let (sender, receiver) = mpsc::sync_channel(bound);
    let mode = get_rr_mode();
    let channel_type = ChannelVariant::MpscBounded;
    let type_name = type_name::<T>().to_string();
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
                type_name,
                channel_variant: channel_type,
                id: id.clone(),
            },
            mode,
            event_recorder: EventRecorder::get_global_recorder(),
        },
        Receiver::new(RealReceiver::Bounded(receiver), id),
    )
}

fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let mode = get_rr_mode();
    let recorder = EventRecorder::get_global_recorder();

    let (sender, receiver) = mpsc::channel();
    let channel_type = ChannelVariant::MpscUnbounded;
    let type_name = type_name::<T>().to_string();
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
                type_name,
                channel_variant: channel_type,
                id: id.clone(),
            },
            mode,
            event_recorder: recorder,
        },
        Receiver::new(RealReceiver::Unbounded(receiver), id),
    )
}

#[cfg(test)]
mod test {
    use crate::mpsc;
    use crate::test;
    use crate::test::rr_test;
    use crate::test::set_rr_mode;
    use crate::test::{Receiver, ReceiverTimeout, Sender, TestChannel, ThreadSafe, TryReceiver};
    use crate::{init_tivo_thread_root, RRMode};
    use anyhow::Result;
    use rusty_fork::rusty_fork_test;
    use std::sync::mpsc::SendError;
    use std::time::Duration;

    enum Mpsc {}
    impl<T: ThreadSafe> Sender<T> for mpsc::Sender<T> {
        type SendError = SendError<T>;

        fn send(&self, msg: T) -> Result<(), Self::SendError> {
            self.send(msg)
        }
    }

    impl<T> Receiver<T> for mpsc::Receiver<T> {
        type RecvError = mpsc::RecvError;

        fn recv(&self) -> Result<T, mpsc::RecvError> {
            self.recv()
        }
    }

    impl<T> TryReceiver<T> for mpsc::Receiver<T> {
        type TryRecvError = mpsc::TryRecvError;

        fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
            self.try_recv()
        }

        const EMPTY: Self::TryRecvError = mpsc::TryRecvError::Empty;
        const TRY_DISCONNECTED: Self::TryRecvError = mpsc::TryRecvError::Disconnected;
    }

    impl<T> ReceiverTimeout<T> for mpsc::Receiver<T> {
        type RecvTimeoutError = mpsc::RecvTimeoutError;

        fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
            self.recv_timeout(timeout)
        }

        const TIMEOUT: Self::RecvTimeoutError = mpsc::RecvTimeoutError::Timeout;
        const DISCONNECTED: Self::RecvTimeoutError = mpsc::RecvTimeoutError::Disconnected;
    }

    impl<T: ThreadSafe> TestChannel<T> for Mpsc {
        type S = mpsc::Sender<T>;
        type R = mpsc::Receiver<T>;

        fn make_channels() -> (Self::S, Self::R) {
            mpsc::channel()
        }
    }

    rusty_fork_test! {
    #[test]
    fn mpsc() -> Result<()> {
        init_tivo_thread_root();
        set_rr_mode(RRMode::NoRR);

        let (_s, _r) = crate::mpsc::channel::<i32>();
        Ok(())
    }

    // #[test]
    // fn mpsc_test_record() -> Result<()> {
    //     init_tivo_thread_root();
    //     test::simple_program::<Mpsc>(RRMode::Record)?;
    //
    //     let reference = test::simple_program_manual_log(
    //         RecordedEvent::MpscSender,
    //         |dti| RecordedEvent::MpscRecvSucc { sender_thread: dti },
    //         ChannelVariant::MpscUnbounded,
    //     );
    //     assert_eq!(reference, get_tl_memory_recorder());
    //     Ok(())
    // }
    //
    // #[test]
    // fn mpsc_test_replay() -> Result<()> {
    //     init_tivo_thread_root();
    //     let reference = test::simple_program_manual_log(
    //         RecordedEvent::MpscSender,
    //         |dti| RecordedEvent::CbRecvSucc { sender_thread: dti },
    //         ChannelVariant::MpscUnbounded,
    //     );
    //     set_tl_memory_recorder(reference);
    //     test::simple_program::<Mpsc>(RRMode::Replay)?;
    //
    //     // Not crashing is the goal, e.g. faithful replay.
    //     // Nothing to assert.
    //     Ok(())
    // }

    // Many of these tests were copied from Mpsc docs!
    #[test]
    fn recv_program_passthrough_test() -> Result<()> {
        init_tivo_thread_root();
        set_rr_mode(RRMode::NoRR);
        test::recv_program::<Mpsc>()?;
        Ok(())
    }

    #[test]
    fn try_recv_program_passthrough_test() -> Result<()> {
        init_tivo_thread_root();
        set_rr_mode(RRMode::NoRR);
        test::try_recv_program::<Mpsc>()?;
        Ok(())
    }

    #[test]
    fn mpsc_test_recv_timeout_passthrough() -> Result<()> {
        init_tivo_thread_root();
        set_rr_mode(RRMode::NoRR);
        test::recv_timeout_program::<Mpsc>()?;
        Ok(())
    }

    #[test]
    fn recv_program_test() -> Result<()> {
        rr_test(test::recv_program::<Mpsc>)
    }

    #[test]
    fn try_recv_program_test() -> Result<()> {
        rr_test(test::try_recv_program::<Mpsc>)
    }

    #[test]
    fn recv_timeout_program_test() -> Result<()> {
        rr_test(test::recv_timeout_program::<Mpsc>)
    }
    }
}
