// pub use crossbeam_channel::{self, RecvError, RecvTimeoutError, SendError, TryRecvError};
use crossbeam_channel as rc; // rc = real_crossbeam
use log::Level::*;
use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::desync::{self, DesyncMode};
use crate::detthread::{self, DetThreadId};
use crate::error::DesyncError;
use crate::recordlog::{self, ChannelVariant, RecordedEvent};
use crate::rr::{self, DetChannelId, SendRecordReplay};
use crate::{BufferedValues, DESYNC_MODE};
use crate::{EventRecorder, RRMode, ENV_LOGGER, RECORD_MODE};

pub use crate::crossbeam_select::{Select, SelectedOperation};
use crate::rr::RecvRecordReplay;
pub use crate::{select, DetMessage};

pub use rc::RecvTimeoutError;
pub use rc::TryRecvError;
pub use rc::{RecvError, SendError};
use std::any::type_name;

pub struct Sender<T> {
    pub(crate) sender: crossbeam_channel::Sender<DetMessage<T>>,
    pub(crate) metadata: recordlog::RecordMetadata,
    mode: RRMode,
    /// Reference to logger of record/replay events. Uses dynamic trait to support multiple
    /// logging implementations.
    event_recorder: EventRecorder,
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

impl<T> SendRecordReplay<T, rc::SendError<T>> for Sender<T> {
    const EVENT_VARIANT: RecordedEvent = RecordedEvent::CbSender;

    fn check_log_entry(&self, entry: RecordedEvent) -> desync::Result<()> {
        match entry {
            RecordedEvent::CbSender => Ok(()),
            log_event => Err(DesyncError::EventMismatch(
                log_event,
                RecordedEvent::CbSender,
            )),
        }
    }

    fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), rc::SendError<T>> {
        self.sender
            .send((thread_id, msg))
            .map_err(|e| rc::SendError(e.into_inner().1))
    }
}

/// Implement crossbeam channel API.
impl<T> Sender<T> {
    /// crossbeam_channel::send implementation.
    pub fn send(&self, msg: T) -> Result<(), rc::SendError<T>> {
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

                        let res = rr::SendRecordReplay::underlying_send(
                            self,
                            detthread::get_forwarding_id(),
                            msg,
                        );
                        res
                    }
                }
            }
        }
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

/// Captures template for: impl RecvRR<_, _> for Receiver<T>
macro_rules! impl_recvrr {
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
                    e => {
                        let mock_event = RecordedEvent::$succ {
                            // TODO: Is there a better value that makes it obvious this is just
                            // a placeholder?
                            sender_thread: DetThreadId::new(),
                        };
                        Err(DesyncError::EventMismatch(e, mock_event))
                    }
                }
            }
        }
    };
}

impl_recvrr!(rc::RecvError, CbRecvSucc, CbRecvErr);
impl_recvrr!(rc::TryRecvError, CbTryRecvSucc, CbTryRecvErr);
impl_recvrr!(rc::RecvTimeoutError, CbRecvTimeoutSucc, CbRecvTimeoutErr);

impl<T> Receiver<T> {
    pub(crate) fn new(real_receiver: ChannelKind<T>, id: DetChannelId) -> Receiver<T> {
        let flavor = Receiver::get_marker(&real_receiver);
        Receiver {
            buffer: RefCell::new(HashMap::new()),
            receiver: real_receiver,
            metadata: recordlog::RecordMetadata::new(type_name::<T>().to_string(), flavor, id),
            recorder: EventRecorder::new_file_recorder(),
            mode: *RECORD_MODE,
        }
    }

    // Implementation of crossbeam_channel public API.

    pub fn recv(&self) -> Result<T, rc::RecvError> {
        let f = || self.receiver.recv();

        self.record_replay(f)
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub fn try_recv(&self) -> Result<T, rc::TryRecvError> {
        let f = || self.receiver.try_recv();

        self.record_replay(f)
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, rc::RecvTimeoutError> {
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
            self.recorder.get_recordable(),
        )
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

    pub(crate) fn metadata(&self) -> &recordlog::RecordMetadata {
        &self.metadata
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

    // The following methods are used for the select operation:

    /// Poll to see if this receiver has this entry. Polling is side-effecty and takes the
    /// message off the channel. The message is sent to the internal buffer to be retreived
    /// later.
    ///
    /// Notice even on `false`, an arbitrary number of messages from _other_ senders may
    /// be buffered.
    pub(crate) fn poll_entry(&self, sender: &DetThreadId) -> bool {
        crate::log_rr!(Debug, "poll_entry()");
        // There is already an entry in the buffer.
        if let Some(queue) = self.buffer.borrow_mut().get(sender) {
            if !queue.is_empty() {
                crate::log_rr!(Debug, "Entry found in buffer");
                return true;
            }
        }

        match self.replay_recv(sender) {
            Ok(msg) => {
                crate::log_rr!(Debug, "Correct message found while polling and buffered.");
                // Save message in buffer for use later.
                self.buffer
                    .borrow_mut()
                    .entry(sender.clone())
                    .or_insert_with(VecDeque::new)
                    .push_back(msg);
                true
            }
            Err(DesyncError::Timedout) => {
                crate::log_rr!(Debug, "No entry found while polling...");
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
    *ENV_LOGGER;

    let id = DetChannelId::new();
    crate::log_rr!(Info, "After channel receiver created: {:?}", id);
    Receiver::new(ChannelKind::After(crossbeam_channel::after(duration)), id)
}

// Chanel constructors provided by crossbeam API:
pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    do_unbounded(EventRecorder::new_file_recorder(), *RECORD_MODE)
}

pub(crate) fn unbounded_in_memory<T>(rr_mode: RRMode) -> (Sender<T>, Receiver<T>) {
    do_unbounded(EventRecorder::new_memory_recorder(), rr_mode)
}

fn do_unbounded<T>(event_recorder: EventRecorder, mode: RRMode) -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let type_name = type_name::<T>().to_string();
    let id = rr::DetChannelId::new();

    crate::log_rr!(Info, "Unbounded channel created: {:?} {:?}", id, type_name);
    (
        Sender {
            sender,
            metadata: recordlog::RecordMetadata::new(
                type_name,
                ChannelVariant::CbUnbounded,
                id.clone(),
            ),
            event_recorder,
            mode,
        },
        Receiver::new(ChannelKind::Unbounded(receiver), id),
    )
}

pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::bounded(cap);
    let id = DetChannelId::new();
    let type_name = type_name::<T>().to_string();

    crate::log_rr!(Info, "Bounded channel created: {:?} {:?}", id, type_name);
    (
        Sender {
            sender,
            metadata: recordlog::RecordMetadata::new(
                type_name,
                ChannelVariant::CbBounded,
                id.clone(),
            ),
            mode: *RECORD_MODE,
            event_recorder: EventRecorder::new_file_recorder(),
        },
        Receiver::new(ChannelKind::Bounded(receiver), id),
    )
}

pub fn never<T>() -> Receiver<T> {
    *ENV_LOGGER;
    let type_name = type_name::<T>().to_string();
    let id = DetChannelId::new();

    Receiver {
        buffer: RefCell::new(HashMap::new()),
        receiver: ChannelKind::Never(crossbeam_channel::never()),
        metadata: recordlog::RecordMetadata {
            type_name,
            channel_variant: ChannelVariant::CbNever,
            id,
        },
        recorder: EventRecorder::new_file_recorder(),
        mode: *RECORD_MODE,
    }
}

/// The tests use thread-local-state. So they should "clash" but for some reason they don't.
/// This might be a problem later. We could use the rusty-fork crate to have every test run on a
/// process instead of thread. We will do this if this ends up being an issue late.
/// My best guess is that every test gets its own brand new thread, which seems very unlikely...
/// but how else?
#[cfg(test)]
mod test {
    use crate::crossbeam_channel::{unbounded, unbounded_in_memory};
    use crate::detthread::get_det_id;
    use crate::recordlog::{ChannelVariant, RecordEntry, RecordedEvent};
    use crate::rr::DetChannelId;
    use crate::{
        get_tl_memory_recorder, init_tivo_thread_root, set_tl_memory_recorder, InMemoryRecorder,
        RRMode,
    };
    use anyhow::Result;
    use crossbeam_channel::{RecvTimeoutError, TryRecvError};
    use std::thread;
    use std::time::Duration;

    fn simple_program(r: RRMode) -> Result<()> {
        // Program which we'll compare logger for.
        let (s, r) = unbounded_in_memory::<i32>(r);
        s.send(1)?;
        s.send(3)?;
        s.send(5)?;
        r.recv()?;
        r.recv()?;
        r.recv()?;
        Ok(())
    }

    fn recv_program(r: RRMode) -> Result<()> {
        let (s, r) = unbounded_in_memory::<i32>(RRMode::Record);
        let _ = s.send(1)?;
        let _ = s.send(2)?;

        assert_eq!(r.recv(), Ok(1));
        assert_eq!(r.recv(), Ok(2));
        Ok(())
    }

    fn try_recv_program(r: RRMode) -> Result<()> {
        let (s, r) = unbounded_in_memory::<i32>(r);
        assert_eq!(r.try_recv(), Err(TryRecvError::Empty));

        s.send(5)?;
        drop(s);

        assert_eq!(r.try_recv(), Ok(5));
        assert_eq!(r.try_recv(), Err(TryRecvError::Disconnected));
        Ok(())
    }

    fn recv_timeout_program(r: RRMode) -> Result<()> {
        let (s, r) = unbounded_in_memory::<i32>(RRMode::Record);

        let h = crate::detthread::spawn::<_, Result<()>>(move || {
            thread::sleep(Duration::from_millis(1));
            s.send(5)?;
            drop(s);
            Ok(())
        });

        assert_eq!(
            r.recv_timeout(Duration::from_micros(500)),
            Err(RecvTimeoutError::Timeout),
        );
        assert_eq!(r.recv_timeout(Duration::from_millis(1)), Ok(5));
        assert_eq!(
            r.recv_timeout(Duration::from_millis(1)),
            Err(RecvTimeoutError::Disconnected),
        );

        // "Unlike with normal errors, this value doesn't implement the Error trait." So we unwrap
        // instead. Not sure why std::thread::Result doesn't impl Result...
        h.join().unwrap()?;
        Ok(())
    }

    #[test]
    fn crossbeam() {
        init_tivo_thread_root();
        let (_s, _r) = unbounded_in_memory::<i32>(RRMode::NoRR);
    }

    #[test]
    fn crossbeam_test_record() {
        init_tivo_thread_root();
        simple_program(RRMode::Record);

        let dti = get_det_id();
        let channel_id = DetChannelId::from_raw(dti.clone(), 1);

        let tn = std::any::type_name::<i32>();
        let re = RecordEntry::new(
            RecordedEvent::CbSender,
            ChannelVariant::CbUnbounded,
            channel_id,
            tn.to_string(),
        );

        let mut reference = InMemoryRecorder::new(dti);
        reference.add_entry(re.clone());
        reference.add_entry(re.clone());
        reference.add_entry(re);

        assert_eq!(reference, get_tl_memory_recorder());
    }

    #[test]
    fn crossbeam_test_replay() {
        fn set_record_log() {
            let dti = get_det_id();
            let channel_id = DetChannelId::from_raw(dti.clone(), 1);

            let tn = std::any::type_name::<i32>();
            let re = RecordEntry::new(
                RecordedEvent::CbSender,
                ChannelVariant::CbUnbounded,
                channel_id,
                tn.to_string(),
            );

            let mut rf: InMemoryRecorder = InMemoryRecorder::new(dti);
            rf.add_entry(re.clone());
            rf.add_entry(re.clone());
            rf.add_entry(re);
            set_tl_memory_recorder(rf);
        }

        init_tivo_thread_root();
        set_record_log();
        simple_program(RRMode::Replay);

        // Not crashing is the goal, e.g. faithful replay.
    }

    // Many of these tests were copied from Crossbeam docs!
    #[test]
    fn crossbeam_test_recv_passthrough() -> Result<()> {
        init_tivo_thread_root();
        recv_program(RRMode::NoRR)?;
        Ok(())
    }

    #[test]
    fn crossbeam_test_recv_record() -> Result<()> {
        init_tivo_thread_root();
        recv_program(RRMode::Record)?;
        Ok(())
    }

    #[test]
    fn crossbeam_test_recv_replay() -> Result<()> {
        init_tivo_thread_root();
        // Record first to fill in-memory recorder.
        recv_program(RRMode::Record)?;
        recv_program(RRMode::Replay)?;
        Ok(())
    }

    #[test]
    fn crossbeam_test_try_recv_record() -> Result<()> {
        init_tivo_thread_root();
        try_recv_program(RRMode::Record)?;
        Ok(())
    }

    #[test]
    fn crossbeam_test_try_recv_passthrough() -> Result<()> {
        init_tivo_thread_root();
        try_recv_program(RRMode::NoRR)?;
        Ok(())
    }

    #[test]
    fn crossbeam_test_try_recv_replay() -> Result<()> {
        init_tivo_thread_root();
        try_recv_program(RRMode::Record)?;
        try_recv_program(RRMode::Replay)?;
        Ok(())
    }

    #[test]
    fn crossbeam_test_recv_timeout_passthrough() -> Result<()> {
        init_tivo_thread_root();
        recv_timeout_program(RRMode::Record)?;
        Ok(())
    }

    #[test]
    fn crossbeam_test_recv_timeout_record() -> Result<()> {
        init_tivo_thread_root();
        recv_timeout_program(RRMode::Record)?;
        Ok(())
    }

    #[test]
    fn crossbeam_test_recv_timeout_replay() -> Result<()> {
        init_tivo_thread_root();
        recv_timeout_program(RRMode::Record)?;
        recv_timeout_program(RRMode::Replay)?;
        Ok(())
    }
}
