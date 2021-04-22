use crossbeam_channel as rc; // rc = real_crossbeam
use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::desync::{self, DesyncMode};
use crate::detthread::{self, get_det_id, DetThreadId};
use crate::error::DesyncError;
use crate::recordlog::{self, ChannelVariant, RecordMetadata, TivoEvent};
use crate::rr::{self, DetChannelId, RecordEventChecker, SendRecordReplay};
use crate::{get_rr_mode, BufferedValues, DESYNC_MODE};
use crate::{EventRecorder, RRMode};

pub use crate::crossbeam_select::{Select, SelectedOperation};
use crate::rr::RecvRecordReplay;
pub use crate::{select, DetMessage};

use crate::fn_basename;
pub use rc::RecvTimeoutError;
pub use rc::TryRecvError;
pub use rc::{RecvError, SendError};
use std::any::type_name;

use tracing::Metadata;
#[allow(unused_imports)]
use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

pub struct Sender<T> {
    pub(crate) sender: crossbeam_channel::Sender<DetMessage<T>>,
    pub(crate) metadata: recordlog::RecordMetadata,
    mode: RRMode,
    /// Reference to logger of record/replay events. Uses dynamic trait to support multiple
    /// logging implementations.
    event_recorder: EventRecorder,
}

impl<T> Sender<T> {
    fn new(
        real_sender: rc::Sender<(DetThreadId, T)>,
        metadata: RecordMetadata,
        mode: RRMode,
        e: EventRecorder,
    ) -> Sender<T> {
        let s = Sender {
            sender: real_sender,
            metadata,
            mode,
            event_recorder: e,
        };
        let _e = s.span(crate::function_name!());
        info!("");
        s
    }

    fn span(&self, fn_name: &str) -> EnteredSpan {
        span!(
            Level::INFO,
            stringify!(CrossbeamSender),
            fn_name,
            "type" = type_name::<T>()
        )
        .entered()
    }

    pub fn send(&self, msg: T) -> Result<(), rc::SendError<T>> {
        let _s = self.span(fn_basename!());

        match self.record_replay_send(msg, &self.mode, &self.metadata, &self.event_recorder) {
            Ok(v) => v,
            // send() should never hang. No need to check if NoEntryLog.
            Err((error, msg)) => {
                // error!(%error);

                match *DESYNC_MODE {
                    DesyncMode::Panic => {
                        panic!("Send::Desynchronization detected: {}", error);
                    }

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

/// crossbeam_channel::Sender does not derive clone. Instead it implements it,
/// this is to avoid the constraint that T must be Clone.
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        // We do not support MPSC for bounded channels as the blocking semantics are
        // more complicated to implement.
        if self.metadata.channel_variant == ChannelVariant::CbBounded {
            panic!(
                "MPSC for bounded channels not supported. Blocking semantics \
                     of bounded channels will not be preserved!"
            )
        }
        Sender {
            sender: self.sender.clone(),
            mode: self.mode,
            metadata: self.metadata.clone(),
            event_recorder: self.event_recorder.clone(),
        }
    }
}

impl<T> RecordEventChecker<rc::SendError<T>> for Sender<T> {
    fn check_recorded_event(&self, re: &TivoEvent) -> Result<(), TivoEvent> {
        match re {
            TivoEvent::CrossbeamSender => Ok(()),
            _ => Err(TivoEvent::CrossbeamSender),
        }
    }
}

impl<T> SendRecordReplay<T, rc::SendError<T>> for Sender<T> {
    const EVENT_VARIANT: TivoEvent = TivoEvent::CrossbeamSender;

    fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), rc::SendError<T>> {
        self.sender
            .send((thread_id, msg))
            .map_err(|e| rc::SendError(e.into_inner().1))
    }
}

/// Holds different channels types which may have slightly different types. This is how
/// crossbeam wraps different types of channels all under the `Receiver<T>` type. So we
/// must do the same to match their API.
pub(crate) enum ChannelKind<T> {
    After(crossbeam_channel::Receiver<T>),
    Bounded(crossbeam_channel::Receiver<DetMessage<T>>),
    Never(crossbeam_channel::Receiver<DetMessage<T>>),
    Unbounded(crossbeam_channel::Receiver<DetMessage<T>>),
}

impl<T> ChannelKind<T> {
    pub(crate) fn try_recv(&self) -> Result<DetMessage<T>, rc::TryRecvError> {
        match self {
            ChannelKind::After(receiver) => {
                match receiver.try_recv() {
                    Ok(msg) => Ok((detthread::get_det_id(), msg)),
                    // TODO: Why is this unreachable?
                    e => e.map(|_| unreachable!()),
                }
            }
            ChannelKind::Bounded(receiver)
            | ChannelKind::Unbounded(receiver)
            | ChannelKind::Never(receiver) => receiver.try_recv(),
        }
    }

    pub(crate) fn recv(&self) -> Result<DetMessage<T>, rc::RecvError> {
        match self {
            ChannelKind::After(receiver) => {
                match receiver.recv() {
                    Ok(msg) => Ok((detthread::get_det_id(), msg)),
                    // TODO: Why is this unreachable?
                    e => e.map(|_| unreachable!()),
                }
            }
            ChannelKind::Bounded(receiver)
            | ChannelKind::Unbounded(receiver)
            | ChannelKind::Never(receiver) => receiver.recv(),
        }
    }

    pub(crate) fn recv_timeout(
        &self,
        duration: Duration,
    ) -> Result<DetMessage<T>, rc::RecvTimeoutError> {
        match self {
            ChannelKind::After(receiver) => match receiver.recv_timeout(duration) {
                Ok(msg) => Ok((detthread::get_det_id(), msg)),
                e => e.map(|_| unreachable!()),
            },
            ChannelKind::Bounded(receiver)
            | ChannelKind::Never(receiver)
            | ChannelKind::Unbounded(receiver) => receiver.recv_timeout(duration),
        }
    }
}

pub struct Receiver<T> {
    /// Buffer holding values from the "wrong" thread on replay mode.
    /// Crossbeam works with inmutable references, so we wrap in a RefCell
    /// to hide our mutation.
    pub(crate) buffer: RefCell<BufferedValues<T>>,
    pub(crate) receiver: ChannelKind<T>,
    pub(crate) metadata: recordlog::RecordMetadata,
    recorder: EventRecorder,
    mode: RRMode,
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

/// Captures template for: impl RecvRR<_, _> for Receiver<T>
macro_rules! impl_recvrr {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> rr::RecvRecordReplay<T, $err_type> for Receiver<T> {
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
                    _ => unreachable!("This should have been checked in RecordEventChecker"),
                }
            }
        }
    };
}

impl_recvrr!(rc::RecvError, CrossbeamRecvSucc, CrossbeamRecvErr);
impl_recvrr!(rc::TryRecvError, CrossbeamTryRecvSucc, CrossbeamTryRecvErr);
impl_recvrr!(
    rc::RecvTimeoutError,
    CrossbeamRecvTimeoutSucc,
    CrossbeamRecvTimeoutErr
);

ImplRecordEventChecker!(rc::RecvError, CrossbeamRecvSucc, CrossbeamRecvErr);
ImplRecordEventChecker!(rc::TryRecvError, CrossbeamTryRecvSucc, CrossbeamTryRecvErr);
ImplRecordEventChecker!(
    rc::RecvTimeoutError,
    CrossbeamRecvTimeoutSucc,
    CrossbeamRecvTimeoutErr
);

impl<T> Receiver<T> {
    pub(crate) fn new(
        mode: RRMode,
        real_receiver: ChannelKind<T>,
        id: DetChannelId,
        recorder: EventRecorder,
    ) -> Receiver<T> {
        let variant = Receiver::get_marker(&real_receiver);

        info!("{}", crate::function_name!());

        Receiver {
            buffer: RefCell::new(HashMap::new()),
            receiver: real_receiver,
            metadata: recordlog::RecordMetadata::new(type_name::<T>().to_string(), variant, id),
            recorder,
            mode,
        }
    }

    // Implementation of crossbeam_channel public API.

    pub fn recv(&self) -> Result<T, rc::RecvError> {
        let _s = self.span(fn_basename!());
        let f = || self.receiver.recv();

        self.record_replay(f)
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub fn try_recv(&self) -> Result<T, rc::TryRecvError> {
        let _s = self.span(fn_basename!());
        let f = || self.receiver.try_recv();

        self.record_replay(f)
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, rc::RecvTimeoutError> {
        let _s = self.span(fn_basename!());
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
        info!("recv()");
        self.record_replay_recv(&self.mode, &self.metadata, g, &self.recorder)
    }

    /// Receives messages from `sender` buffering all other messages which it gets. Times out after
    /// 1 second (this value can easily be changed). Used by crossbeam select function, and
    /// implementing ReceiverRR for this type.
    pub(crate) fn replay_recv(&self, sender: &DetThreadId) -> desync::Result<T> {
        rr::recv_expected_message(
            &sender,
            || {
                self.receiver
                    .recv_timeout(Duration::from_secs(1))
                    .map_err(|e| e.into())
            },
            &mut self.get_buffer(),
        )
    }

    pub(crate) fn get_buffer(&self) -> RefMut<BufferedValues<T>> {
        self.buffer.borrow_mut()
    }

    pub(crate) fn get_marker(receiver: &ChannelKind<T>) -> ChannelVariant {
        match receiver {
            ChannelKind::Unbounded(_) => ChannelVariant::CbUnbounded,
            ChannelKind::After(_) => ChannelVariant::CbAfter,
            ChannelKind::Bounded(_) => ChannelVariant::CbBounded,
            ChannelKind::Never(_) => ChannelVariant::CbNever,
        }
    }

    /// Get a message that might have been buffered during replay. Useful for in case of
    /// desynchronization when message replay no longer matters.
    pub(crate) fn get_buffered_value(&self) -> Option<T> {
        let mut hashmap = self.buffer.borrow_mut();
        for queue in hashmap.values_mut() {
            if let v @ Some(_) = queue.pop_front() {
                return v;
            }
        }
        None
    }

    fn span(&self, fn_name: &str) -> EnteredSpan {
        span!(
            Level::INFO,
            stringify!(Receiver),
            fn_name,
            "type" = type_name::<T>()
        )
        .entered()
    }

    // The following methods are used for the select operation:

    /// Poll to see if this receiver has this entry. Polling is side-effecty and takes the
    /// message off the channel. The message is sent to the internal buffer to be retreived
    /// later.
    ///
    /// Notice even on `false`, an arbitrary number of messages from _other_ senders may
    /// be buffered.
    pub(crate) fn poll_entry(&self, sender: &DetThreadId) -> bool {
        let _s = self.span(fn_basename!());

        // There is already an entry in the buffer.
        if let Some(queue) = self.buffer.borrow_mut().get(sender) {
            if !queue.is_empty() {
                debug!("Entry found in buffer");
                return true;
            }
        }

        match self.replay_recv(sender) {
            Ok(msg) => {
                debug!("Correct message found while polling and buffered.");
                // Save message in buffer for use later.
                self.buffer
                    .borrow_mut()
                    .entry(sender.clone())
                    .or_insert_with(VecDeque::new)
                    .push_back(msg);
                true
            }
            Err(DesyncError::Timeout(t)) => {
                debug!("Timed out while waiting for message of type: {:?}", t);
                false
            }
            // TODO document why this is unreachable.
            _ => unreachable!(),
        }
    }
}

// We need to make sure our ENV_LOGGER is initialized. We do this when the user of this library
// creates channels. Below are the "entry points" to creating channels.
pub fn after(duration: Duration) -> Receiver<Instant> {
    let id = DetChannelId::generate_new_unique_channel_id();
    let recorder = EventRecorder::get_global_recorder();

    let _s = span!(Level::INFO, stringify!(after)).entered();
    Receiver::new(
        get_rr_mode(),
        ChannelKind::After(crossbeam_channel::after(duration)),
        id,
        recorder,
    )
}

// Chanel constructors provided by crossbeam API:
pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    let recorder = EventRecorder::get_global_recorder();
    let mode = get_rr_mode();

    let (sender, receiver) = crossbeam_channel::unbounded();
    let type_name = type_name::<T>().to_string();
    let id = rr::DetChannelId::generate_new_unique_channel_id();

    let metadata =
        recordlog::RecordMetadata::new(type_name, ChannelVariant::CbUnbounded, id.clone());

    let _s = span!(
        Level::INFO,
        "New Channel Created",
        dti = ?get_det_id(),
        variant = ?metadata.channel_variant,
        chan=%id
    )
    .entered();

    (
        Sender::new(sender, metadata, mode, recorder.clone()),
        Receiver::new(mode, ChannelKind::Unbounded(receiver), id, recorder),
    )
}

pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    // *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::bounded(cap);
    let id = DetChannelId::generate_new_unique_channel_id();

    let type_name = type_name::<T>().to_string();

    let recorder = EventRecorder::get_global_recorder();
    let metadata = recordlog::RecordMetadata::new(type_name, ChannelVariant::CbBounded, id.clone());
    (
        Sender::new(sender, metadata, get_rr_mode(), recorder.clone()),
        Receiver::new(get_rr_mode(), ChannelKind::Bounded(receiver), id, recorder),
    )
}

pub fn never<T>() -> Receiver<T> {
    let id = DetChannelId::generate_new_unique_channel_id();

    Receiver::new(
        get_rr_mode(),
        ChannelKind::Never(crossbeam_channel::never()),
        id,
        EventRecorder::get_global_recorder(),
    )
}

/// The tests use thread-local-state. So they should "clash" but for some reason they don't.
/// This might be a problem later. We could use the rusty-fork crate to have every test run on a
/// process instead of thread. We will do this if this ends up being an issue late.
/// My best guess is that every test gets its own brand new thread, which seems very unlikely...
/// but how else?
#[cfg(test)]
mod tests {
    use crate::crossbeam_channel as cb;
    use crate::recordlog::take_global_memory_recorder;
    use crate::recordlog::{ChannelVariant, TivoEvent};
    use crate::test;
    use crate::test::{
        rr_test, set_rr_mode, Receiver, ReceiverTimeout, Sender, TestChannel, ThreadSafe,
        TryReceiver,
    };
    use crate::RRMode;
    use crate::Tivo;
    use anyhow::Result;
    use rusty_fork::rusty_fork_test;

    use crate::crossbeam_channel::unbounded;
    use std::time::Duration;

    pub(crate) struct Crossbeam {}

    impl<T: ThreadSafe> TestChannel<T> for Crossbeam {
        type S = cb::Sender<T>;
        type R = cb::Receiver<T>;

        fn make_channels() -> (Self::S, Self::R) {
            unbounded()
        }
    }

    impl<T: ThreadSafe> Sender<T> for cb::Sender<T> {
        type SendError = cb::SendError<T>;
        fn send(&self, msg: T) -> Result<(), cb::SendError<T>> {
            self.send(msg)
        }
    }

    impl<T> Receiver<T> for cb::Receiver<T> {
        type RecvError = cb::RecvError;

        fn recv(&self) -> Result<T, cb::RecvError> {
            self.recv()
        }
    }

    impl<T> TryReceiver<T> for cb::Receiver<T> {
        type TryRecvError = cb::TryRecvError;

        fn try_recv(&self) -> Result<T, cb::TryRecvError> {
            self.try_recv()
        }

        const EMPTY: Self::TryRecvError = cb::TryRecvError::Empty;
        const TRY_DISCONNECTED: Self::TryRecvError = cb::TryRecvError::Disconnected;
    }

    impl<T> ReceiverTimeout<T> for cb::Receiver<T> {
        type RecvTimeoutError = cb::RecvTimeoutError;

        fn recv_timeout(&self, timeout: Duration) -> Result<T, Self::RecvTimeoutError> {
            self.recv_timeout(timeout)
        }

        const TIMEOUT: Self::RecvTimeoutError = cb::RecvTimeoutError::Timeout;
        const DISCONNECTED: Self::RecvTimeoutError = cb::RecvTimeoutError::Disconnected;
    }

    fn after_always_timeout() -> Result<()> {
        let (_, r) = cb::unbounded::<i32>();
        let timeout = Duration::from_millis(1);

        crate::select! {
            recv(r) -> msg => println!("received {:?}", msg),
            recv(cb::after(timeout)) -> _ => println!("timed out"),
        }

        Ok(())
    }

    rusty_fork_test! {
    #[test]
    fn init_unbounded() {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);
        let (_s, _r) = unbounded::<i32>();
    }

    #[test]
    fn simple_program_record_test() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::Record);
        test::simple_program::<Crossbeam>()?;

        let reference = test::simple_program_manual_log(
            TivoEvent::CrossbeamSender,
            |dti| TivoEvent::CrossbeamRecvSucc { sender_thread: dti },
            ChannelVariant::CbUnbounded,
        );
        assert_eq!(reference, take_global_memory_recorder());
        Ok(())
    }

    #[test]
    fn try_recv_passthrough_test() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);
        test::try_recv_program::<Crossbeam>()?;
        Ok(())
    }

    #[test]
    fn recv_timeout_passthrough_test() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);
        test::recv_timeout_program::<Crossbeam>()?;
        Ok(())
    }

    // Many of these tests were copied from Crossbeam docs!

    #[test]
    fn recv_program_test() -> Result<()> {
            tracing_subscriber::fmt::Subscriber::builder()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_target(false)
            .without_time()
            .init();
        rr_test(test::recv_program::<Crossbeam>)
    }

    #[test]
    fn try_recv_program_test() -> Result<()> {
        rr_test(test::try_recv_program::<Crossbeam>)
    }

    #[test]
    fn recv_timeout_program_test() -> Result<()> {
        rr_test(test::recv_timeout_program::<Crossbeam>)
    }

    #[test]
    fn after_always_timeout_test() -> Result<()> {
        rr_test(after_always_timeout)
    }
    }
}
