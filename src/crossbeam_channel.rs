// pub use crossbeam_channel::{self, RecvError, RecvTimeoutError, SendError, TryRecvError};
use crossbeam_channel as rc; // rc = real_crossbeam
use log::Level::*;
use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::desync::{self, DesyncMode};
use crate::detthread::{self, DetThreadId};
use crate::error::DesyncError;
use crate::recordlog::{self, ChannelLabel, RecordedEvent};
use crate::rr::{self, DetChannelId, SendRecordReplay};
use crate::{get_generic_name, ENV_LOGGER, RECORD_MODE};
use crate::{DESYNC_MODE, BufferedValues};

pub use crate::crossbeam_select::{Select, SelectedOperation};
use crate::rr::RecvRecordReplay;
pub use crate::{select, DetMessage};

pub use rc::RecvTimeoutError;
pub use rc::TryRecvError;
pub use rc::{RecvError, SendError};

pub struct Sender<T> {
    pub(crate) sender: crossbeam_channel::Sender<DetMessage<T>>,
    pub(crate) metadata: recordlog::RecordMetadata,
}

/// crossbeam_channel::Sender does not derive clone. Instead it implements it,
/// this is to avoid the constraint that T must be Clone.
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        // We do not support MPSC for bounded channels as the blocking semantics are
        // more complicated to implement.
        if self.metadata.flavor == ChannelLabel::Bounded {
            crate::log_rr!(
                Warn,
                "MPSC for bounded channels not supported. Blocking semantics \
                     of bounded channels will not be preserved!"
            );
        }
        Sender {
            sender: self.sender.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

impl<T> SendRecordReplay<T, rc::SendError<T>> for Sender<T> {
    const EVENT_VARIANT: RecordedEvent = RecordedEvent::Sender;

    fn check_log_entry(&self, entry: RecordedEvent) -> desync::Result<()> {
        match entry {
            RecordedEvent::Sender => Ok(()),
            log_event => Err(DesyncError::EventMismatch(log_event, RecordedEvent::Sender)),
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
            &self.metadata,
            "CrossbeamSender",
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

/// Holds different channels types which may have slightly different types. This is how
/// crossbeam wraps different types of channels all under the `Receiver<T>` type. So we
/// must do the same to match their API.
pub(crate) enum ChannelVariant<T> {
    After(crossbeam_channel::Receiver<T>),
    Bounded(crossbeam_channel::Receiver<DetMessage<T>>),
    Never(crossbeam_channel::Receiver<DetMessage<T>>),
    Unbounded(crossbeam_channel::Receiver<DetMessage<T>>),
}

impl<T> ChannelVariant<T> {
    pub(crate) fn try_recv(&self) -> Result<DetMessage<T>, rc::TryRecvError> {
            match self {
                ChannelVariant::After(receiver) => {
                    match receiver.try_recv() {
                        Ok(msg) => Ok((detthread::get_det_id(), msg)),
                        // TODO: Why is this unreachable?
                        e => e.map(|_| unreachable!()),
                    }
                }
                ChannelVariant::Bounded(receiver)
                | ChannelVariant::Unbounded(receiver)
                | ChannelVariant::Never(receiver) => receiver.try_recv(),
            }
    }

    pub(crate) fn recv(&self) -> Result<DetMessage<T>, rc::RecvError> {
            match self {
                ChannelVariant::After(receiver) => {
                    match receiver.recv() {
                        Ok(msg) => Ok((detthread::get_det_id(), msg)),
                        // TODO: Why is this unreachable?
                        e => e.map(|_| unreachable!()),
                    }
                }
                ChannelVariant::Bounded(receiver)
                | ChannelVariant::Unbounded(receiver)
                | ChannelVariant::Never(receiver) => receiver.recv(),
            }
        }

    pub(crate) fn recv_timeout(
        &self,
        duration: Duration,
    ) -> Result<DetMessage<T>, rc::RecvTimeoutError> {
        match self {
            ChannelVariant::After(receiver) => match receiver.recv_timeout(duration) {
                Ok(msg) => Ok((detthread::get_det_id(), msg)),
                e => e.map(|_| unreachable!()),
            },
            ChannelVariant::Bounded(receiver)
            | ChannelVariant::Never(receiver)
            | ChannelVariant::Unbounded(receiver) => receiver.recv_timeout(duration),
        }
    }
}

pub struct Receiver<T> {
    /// Buffer holding values from the "wrong" thread on replay mode.
    /// Crossbeam works with inmutable references, so we wrap in a RefCell
    /// to hide our mutation.
    pub(crate) buffer: RefCell<BufferedValues<T>>,
    pub(crate) receiver: ChannelVariant<T>,
    pub(crate) metadata: recordlog::RecordMetadata,
}

/// Captures template for: impl RecvRR<_, _> for Receiver<T>
macro_rules! impl_recvrr {
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

impl_recvrr!(rc::RecvError, RecvSucc, RecvErr);
impl_recvrr!(rc::TryRecvError, TryRecvSucc, TryRecvErr);
impl_recvrr!(rc::RecvTimeoutError, RecvTimeoutSucc, RecvTimeoutErr);

impl<T> Receiver<T> {
    // Implementation of crossbeam_channel public API.

    pub fn recv(&self) -> Result<T, rc::RecvError> {
        let receiver = || self.receiver.recv();
        RecvRecordReplay::record_replay_with(self, self.metadata(), receiver, "channel::recv()")
            .unwrap_or_else(|e| desync::handle_desync(e, receiver, self.get_buffer()))
    }

    pub fn try_recv(&self) -> Result<T, rc::TryRecvError> {
        let f = || self.receiver.try_recv();
        self.record_replay_with(self.metadata(), f, "channel::try_recv()")
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, rc::RecvTimeoutError> {
        let f = || self.receiver.recv_timeout(timeout);
        self.record_replay_with(self.metadata(), f, "channel::rect_timeout()")
            .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
    }

    // Internal methods necessary for record and replaying.

    /// Receives messages from `sender` buffering all other messages which it gets.
    /// Times out after 1 second (this value can easily be changed).
    /// Used by crossbeam select function, and implementing ReceiverRR for this type.
    pub(crate) fn replay_recv(&self, sender: &DetThreadId) -> desync::Result<T> {
        rr::recv_expected_message(
            &sender,
            || self.receiver.recv_timeout(Duration::from_secs(1)).map_err(|e| e.into()),
            self.get_buffer(),
            &self.metadata.id,
        )
    }

    pub(crate) fn new(real_receiver: ChannelVariant<T>, id: DetChannelId) -> Receiver<T> {
        let flavor = Receiver::get_marker(&real_receiver);
        Receiver {
            buffer: RefCell::new(HashMap::new()),
            receiver: real_receiver,
            metadata: recordlog::RecordMetadata::new(
                get_generic_name::<T>().to_string(),
                flavor,
                *RECORD_MODE,
                id)
        }
    }

    pub(crate) fn get_buffer(&self) -> RefMut<BufferedValues<T>> {
        self.buffer.borrow_mut()
    }

    pub(crate) fn metadata(&self) -> &recordlog::RecordMetadata {
        &self.metadata
    }

    pub(crate) fn get_marker(receiver: &ChannelVariant<T>) -> ChannelLabel {
        match receiver {
            ChannelVariant::Unbounded(_) => ChannelLabel::Unbounded,
            ChannelVariant::After(_) => ChannelLabel::After,
            ChannelVariant::Bounded(_) => ChannelLabel::Bounded,
            ChannelVariant::Never(_) => ChannelLabel::Never,
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
                    .or_insert(VecDeque::new())
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
    Receiver::new(
        ChannelVariant::After(crossbeam_channel::after(duration)),
        id,
    )
}

// Chanel constructors provided by crossbeam API:
pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let type_name = get_generic_name::<T>();
    let id = rr::DetChannelId::new();

    crate::log_rr!(Info, "Unbounded channel created: {:?} {:?}", id, type_name);
    (
        Sender {
            sender,
            metadata: recordlog::RecordMetadata::new(
                type_name.to_string(),
                ChannelLabel::Unbounded,
                *RECORD_MODE,
                id.clone())
        },
        Receiver::new(ChannelVariant::Unbounded(receiver), id),
    )
}

pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::bounded(cap);
    let id = DetChannelId::new();
    let type_name = get_generic_name::<T>();

    crate::log_rr!(Info, "Bounded channel created: {:?} {:?}", id, type_name);
    (
        Sender {
            sender,
            metadata: recordlog::RecordMetadata::new(
                type_name.to_string(),
                ChannelLabel::Bounded,
                *RECORD_MODE,
                id.clone())
        },
        Receiver::new(ChannelVariant::Bounded(receiver), id),
    )
}

pub fn never<T>() -> Receiver<T> {
    *ENV_LOGGER;
    let type_name = get_generic_name::<T>().to_string();
    let id = DetChannelId::new();

    Receiver {
        buffer: RefCell::new(HashMap::new()),
        receiver: ChannelVariant::Never(crossbeam_channel::never()),
        metadata: recordlog::RecordMetadata {
            type_name,
            flavor: ChannelLabel::Never,
            mode: *RECORD_MODE,
            id,
        },
    }
}
