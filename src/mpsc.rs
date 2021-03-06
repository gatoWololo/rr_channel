use std::cell::RefCell;
use std::cell::RefMut;
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use crate::detthread::{get_det_id, DetThreadId};
use crate::error::RecvErrorRR;
use crate::recordlog::{self, ChannelVariant, TivoEvent};
use crate::rr::SendRecordReplay;
use crate::rr::{self, DetChannelId};
use crate::rr::{RecordEventChecker, RecvRecordReplay};
use crate::{desync, get_rr_mode, rr_channel_creation_event, EventRecorder, RRMode};
use crate::{BufferedValues, DESYNC_MODE};
use crate::{DesyncMode, DetMessage};
use std::any::type_name;

#[allow(unused_imports)]
use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

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

macro_rules! ImplRecordEventChecker {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> RecordEventChecker<$err_type> for Receiver<T> {
            fn check_recorded_event(&self, re: &TivoEvent) -> Result<(), TivoEvent> {
                match re {
                    TivoEvent::$succ { sender_thread: _ } => Ok(()),
                    TivoEvent::$err(_) => Ok(()),
                    _ => Err(TivoEvent::$succ {
                        sender_thread: DetThreadId::new(),
                    }),
                }
            }
        }
    };
}

/// General template for implementing trait RecvRR for our mpsc::Receiver<T>.
macro_rules! impl_RR {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> RecvRecordReplay<T, $err_type> for Receiver<T> {
            fn recorded_event_succ(dtid: DetThreadId) -> recordlog::TivoEvent {
                TivoEvent::$succ {
                    sender_thread: dtid,
                }
            }

            fn recorded_event_err(e: &$err_type) -> recordlog::TivoEvent {
                TivoEvent::$err(*e)
            }

            fn replay_recorded_event(
                &self,
                event: TivoEvent,
            ) -> desync::Result<Result<T, $err_type>> {
                match event {
                    TivoEvent::$succ { sender_thread } => {
                        let retval = self.replay_recv(&sender_thread)?;
                        Ok(Ok(retval))
                    }
                    TivoEvent::$err(e) => {
                        trace!("Creating error event for: {:?}", TivoEvent::$err(e));
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

ImplRecordEventChecker!(mpsc::RecvError, MpscRecvSucc, MpscRecvErr);
ImplRecordEventChecker!(mpsc::TryRecvError, MpscTryRecvSucc, MpscTryRecvErr);
ImplRecordEventChecker!(
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
    fn new(
        real_receiver: RealReceiver<T>,
        id: DetChannelId,
        mode: RRMode,
        event_recorder: EventRecorder,
    ) -> Receiver<T> {
        let flavor = Receiver::get_marker(&real_receiver);
        Receiver {
            buffer: RefCell::new(HashMap::new()),
            receiver: real_receiver,
            metadata: recordlog::RecordMetadata {
                type_name: type_name::<T>().to_string(),
                channel_variant: flavor,
                id,
            },
            event_recorder,
            mode,
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
        self.record_replay_recv(&self.mode, &self.metadata, g, &self.event_recorder)
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
        match self.record_replay_send(msg, &self.mode, &self.metadata, &self.event_recorder) {
            Ok(v) => v,
            // send() should never hang. No need to check if NoEntryLog.
            Err((error, msg)) => {
                warn!("Desynchronization detected: {:?}", error);
                match *DESYNC_MODE {
                    DesyncMode::Panic => panic!("Send::Desynchronization detected: {:?}", error),
                    // TODO: One day we may want to record this alternate execution.
                    DesyncMode::KeepGoing => {
                        desync::mark_program_as_desynced();
                        rr::SendRecordReplay::underlying_send(self, get_det_id(), msg)
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
            warn!(
                "MPSC for bounded channels not supported. Blocking semantics \
                     of bounded channels will not be preserved!"
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

impl<T> RecordEventChecker<mpsc::SendError<T>> for Sender<T> {
    fn check_recorded_event(&self, re: &TivoEvent) -> Result<(), TivoEvent> {
        match re {
            TivoEvent::MpscSender => Ok(()),
            _ => Err(TivoEvent::MpscSender),
        }
    }
}

impl<T> SendRecordReplay<T, mpsc::SendError<T>> for Sender<T> {
    const EVENT_VARIANT: TivoEvent = TivoEvent::MpscSender;

    fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), mpsc::SendError<T>> {
        self.sender.send((thread_id, msg))
    }
}

impl<T> RealSender<T> {
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
    let (sender, receiver) = mpsc::sync_channel(bound);
    let mode = get_rr_mode();
    let channel_type = ChannelVariant::MpscBounded;
    let type_name = type_name::<T>().to_string();
    let id = DetChannelId::generate_new_unique_channel_id();
    let recorder = EventRecorder::get_global_recorder();

    info!("Bounded mpsc channel created: {:?} {:?}", id, type_name);

    // Cant't really propagate error up.. panic!
    if let Err(e) =
        rr_channel_creation_event::<T>(mode, &id, &recorder, ChannelVariant::MpscBounded)
    {
        error!("{}", e);
        panic!("{}", e);
    }

    (
        Sender {
            sender: RealSender::Bounded(sender),
            metadata: recordlog::RecordMetadata {
                type_name,
                channel_variant: channel_type,
                id: id.clone(),
            },
            mode,
            event_recorder: recorder.clone(),
        },
        Receiver::new(RealReceiver::Bounded(receiver), id, mode, recorder),
    )
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let mode = get_rr_mode();
    let recorder = EventRecorder::get_global_recorder();

    let (sender, receiver) = mpsc::channel();
    let channel_type = ChannelVariant::MpscUnbounded;
    let type_name = type_name::<T>().to_string();
    let id = DetChannelId::generate_new_unique_channel_id();

    // Cant't really propagate error up.. panic!
    if let Err(e) =
        rr_channel_creation_event::<T>(mode, &id, &recorder, ChannelVariant::MpscUnbounded)
    {
        error!("{}", e);
        panic!("{}", e);
    }

    info!("Unbounded mpsc channel created: {:?} {:?}", id, type_name);

    (
        Sender {
            sender: RealSender::Unbounded(sender),
            metadata: recordlog::RecordMetadata {
                type_name,
                channel_variant: channel_type,
                id: id.clone(),
            },
            mode,
            event_recorder: recorder.clone(),
        },
        Receiver::new(RealReceiver::Unbounded(receiver), id, mode, recorder),
    )
}

#[cfg(test)]
mod test {
    use crate::mpsc;
    use crate::test;
    use crate::test::rr_test;
    use crate::test::set_rr_mode;
    use crate::test::{Receiver, ReceiverTimeout, Sender, TestChannel, ThreadSafe, TryReceiver};
    use crate::RRMode;
    use crate::Tivo;
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
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);

        let (_s, _r) = crate::mpsc::channel::<i32>();
        Ok(())
    }

    // Many of these tests were copied from Mpsc docs!
    #[test]
    fn recv_program_passthrough_test() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);
        test::recv_program::<Mpsc>()?;
        Ok(())
    }

    #[test]
    fn try_recv_program_passthrough_test() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);
        test::try_recv_program::<Mpsc>()?;
        Ok(())
    }

    #[test]
    fn mpsc_test_recv_timeout_passthrough() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
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
