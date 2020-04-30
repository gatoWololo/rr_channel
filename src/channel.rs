use crate::get_generic_name;
use crate::rr::{self, ChannelLabel, RecordedEvent,
                           RecordMetadata, RecvRR, SendRR,
                           DetChannelId, DesyncError, get_forward_id,
                           RecvErrorRR};
use crate::thread::get_and_inc_channel_id;
use crate::thread::*;
use crate::{RRMode, ENV_LOGGER, RECORD_MODE, DesyncMode, DESYNC_MODE};
use crossbeam_channel::{self, RecvError, SendError, RecvTimeoutError, TryRecvError};
use log::Level::*;
use crate::rr::mark_program_as_desynced;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::error::Error;
use std::time::Duration;
use std::time::Instant;
use std::cell::RefMut;
use crate::log_rr;

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let mode = *RECORD_MODE;
    let channel_type = ChannelLabel::Unbounded;
    let type_name = get_generic_name::<T>();
    let id = DetChannelId::new();

    log_rr!(Info, "Unbounded channel created: {:?} {:?}", id, type_name);
    (
        Sender {
            sender,
            mode,
            channel_type,
            channel_id: id.clone(),
            type_name: type_name.to_string(),
        },
        Receiver::new(ChannelVariant::Unbounded(receiver), id),
    )
}

/// TODO: Switch all these individual fields and use metadata instead.
pub struct Sender<T> {
    pub(crate) sender: crossbeam_channel::Sender<(Option<DetThreadId>, T)>,
    // Unlike Receiver, whose channels may have different types, the Sender always
    // has the same channel type. So we just keep a marker around.
    pub(crate) channel_type: ChannelLabel,
    mode: RRMode,
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
        if self.channel_type == ChannelLabel::Bounded {
            log_rr!(Warn,
                    "MPSC for bounded channels not supported. Blocking semantics \
                     of bounded channels will not be preseved!");
        }
        Sender {
            sender: self.sender.clone(),
            mode: self.mode.clone(),
            channel_type: self.channel_type,
            channel_id: self.channel_id.clone(),
            type_name: self.type_name.clone(),
        }
    }
}

impl<T> SendRR<T, SendError<T>> for Sender<T> {
    fn check_log_entry(&self, entry: RecordedEvent) -> Result<(), DesyncError> {
        match entry {
            RecordedEvent::Sender => Ok(()),
            log_event => Err(DesyncError::EventMismatch(log_event, RecordedEvent::Sender)),
        }
    }

    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), SendError<T>> {
        self.sender
            .send((thread_id, msg))
            .map_err(|e| SendError(e.into_inner().1))
    }

    const EVENT_VARIANT: RecordedEvent = RecordedEvent::Sender;
}

/// Implement crossbeam channel API.
impl<T> Sender<T> {
    // Send our det thread id along with the actual message for both record and replay.
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        match self.rr_send(
            msg,
            &self.mode,
            &self.channel_id,
            &self.type_name,
            &self.channel_type,
            "CrossbeamSender",
        ) {
            Ok(v) => v,
            // send() should never hang. No need to check if NoEntryLog.
            Err((error, msg)) => {
                log_rr!(Warn, "Desynchronization detected: {:?}", error);

                match *DESYNC_MODE {
                    DesyncMode::Panic =>
                        panic!("Send::Desynchronization detected: {:?}", error),

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

/// Holds different channels types which may have slightly different types.
/// This is just the complexity cost of piggybacking off crossbeam_channels
/// and their API so much.
pub enum ChannelVariant<T> {
    After(crossbeam_channel::Receiver<T>),
    Bounded(crossbeam_channel::Receiver<(Option<DetThreadId>, T)>),
    Never(crossbeam_channel::Receiver<(Option<DetThreadId>, T)>),
    Unbounded(crossbeam_channel::Receiver<(Option<DetThreadId>, T)>),
}

/// Helper macro to generate methods for ChannelVariant. Calls appropriate channel method.
/// On case of ChannelVariant::After inserts get_det_id() as the sender_thread_id in order
/// to have the correct type.
macro_rules! generate_channel_method {
    ($method:ident, $error_type:ty) => {
        pub fn $method(&self) -> Result<(Option<DetThreadId>, T), $error_type> {
            match self {
                ChannelVariant::After(receiver) => {
                    match receiver.$method() {
                        Ok(msg) => Ok((get_det_id(), msg)),
                        // TODO: Why is this unreachable?
                        e => e.map(|_| unreachable!()),
                    }
                }
                ChannelVariant::Bounded(receiver) |
                ChannelVariant::Unbounded(receiver) |
                ChannelVariant::Never(receiver) => receiver.$method(),
            }
        }
    }
}

impl<T> ChannelVariant<T> {
    generate_channel_method!(try_recv, TryRecvError);
    generate_channel_method!(recv, RecvError);

    pub fn recv_timeout(
        &self,
        duration: Duration,
    ) -> Result<(Option<DetThreadId>, T), RecvTimeoutError> {
         match self {
            ChannelVariant::After(receiver) => match receiver.recv_timeout(duration) {
                Ok(msg) => Ok((get_det_id(), msg)),
                e => e.map(|_| unreachable!()),
            },
             ChannelVariant::Bounded(receiver) |
             ChannelVariant::Never(receiver) |
             ChannelVariant::Unbounded(receiver) => {
                receiver.recv_timeout(duration)
            }
        }
    }
}

pub struct Receiver<T> {
    /// Buffer holding values from the "wrong" thread on replay mode.
    /// Crossbeam works with inmutable references, so we wrap in a RefCell
    /// to hide our mutation.
    pub(crate) buffer: RefCell<HashMap<Option<DetThreadId>, VecDeque<T>>>,
    pub(crate) receiver: ChannelVariant<T>,
    pub(crate) metadata: RecordMetadata,
}

macro_rules! impl_RR {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> RecvRR<T, $err_type> for Receiver<T> {
            fn to_recorded_event(
                &self,
                event: Result<(Option<DetThreadId>, T), $err_type>,
            ) -> (Result<T, $err_type>, RecordedEvent) {
                match event {
                    Ok((sender_thread, msg)) => {
                        (Ok(msg), RecordedEvent::$succ { sender_thread })
                    }
                    Err(e) => {
                        (Err(e), RecordedEvent::$err(e))
                    }
                }
            }

            fn expected_recorded_events(&self, event: RecordedEvent)
                                        -> Result<Result<T, $err_type>, DesyncError> {
                match event {
                    RecordedEvent::$succ { sender_thread } => {
                        let retval = self.replay_recv(&sender_thread)?;
                        // Here is where we explictly increment our event_id!
                        inc_event_id();
                        Ok(Ok(retval))
                    }
                    RecordedEvent::$err(e) => {
                        log_rr!(Trace, "Creating error event for: {:?}", RecordedEvent::$err(e));
                        // Here is where we explictly increment our event_id!
                        inc_event_id();
                        Ok(Err(e))
                    }
                    e => {
                        let mock_event = RecordedEvent::$succ { sender_thread: None };
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

impl_RR!(RecvError, RecvSucc, RecvErr);
impl_RR!(TryRecvError, TryRecvSucc, TryRecvErr);
impl_RR!(RecvTimeoutError, RecvTimeoutSucc, RecvTimeoutErr);


/// Implement crossbeam channel API.
impl<T> Receiver<T> {
    pub fn new(real_receiver: ChannelVariant<T>, id: DetChannelId) -> Receiver<T> {
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

    /// Fetch buffered value if any.
    pub(crate) fn get_buffered_value(&self) -> Option<T> {
        let mut hashmap = self.buffer.borrow_mut();
        for queue in hashmap.values_mut() {
            if let v@Some(_) = queue.pop_front() {
                return v;
            }
        }
        None
    }

    /// Poll to see if this receiver has this entry. Polling is side-effecty and takes the
    /// message off the channel. The message is sent to the internal buffer to be retreived
    /// later.
    ///
    /// Notice even on `false`, an arbitrary number of messages from _other_ senders may
    /// be buffered.
    pub(crate) fn poll_entry(&self, sender: &Option<DetThreadId>, timeout: Duration)
                             -> bool {
        log_rr!(Debug, "poll_entry()");
        // There is already an entry in the buffer.
        if let Some(queue) = self.buffer.borrow_mut().get(sender) {
            if ! queue.is_empty() {
                log_rr!(Debug, "Entry found in buffer");
                return true;
            }
        }

        match self.replay_recv_timeout(sender, timeout) {
            Ok(msg) => {
                log_rr!(Debug, "Correct message found while polling and buffered.");
                // Save message in buffer for use later.
                self.buffer.borrow_mut().entry(sender.clone()).
                    or_insert(VecDeque::new()).
                    push_back(msg);
                true
            }
            Err(DesyncError::Timedout) => {
                log_rr!(Debug, "No entry found while polling...");
                false
            }
            _ => unreachable!(),
        }
    }

    /// This is the raw function called in a loop by our rr trait.
    /// It should not be called directly by anyone, as there is buffering
    /// that will not be properly taken into account if called directly.
    fn rr_recv_timeout(&self, timeout: Duration)
                       -> Result<(Option<DetThreadId>, T), RecvErrorRR> {
        self.receiver.recv_timeout(timeout).
            map_err(|e| match e {
                RecvTimeoutError::Timeout => RecvErrorRR::Timeout,
                RecvTimeoutError::Disconnected => RecvErrorRR::Disconnected,
            })
    }

    pub(crate) fn replay_recv(&self, sender: &Option<DetThreadId>) -> Result<T, DesyncError> {
        self.replay_recv_timeout(sender, Duration::from_secs(1))
    }

    pub(crate) fn replay_recv_timeout(&self, sender: &Option<DetThreadId>, timeout: Duration)
                                      -> Result<T, DesyncError> {
        // Huh weirdly it doesn't matter which error type we pass in to RecvRR here... Which
        // makes sense since they all
        rr::recv_from_sender(
            &sender,
            || self.rr_recv_timeout(timeout),
            &mut self.buffer.borrow_mut(),
            &self.metadata.id,
        )
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        let f = || self.receiver.recv();
        self.rr_recv(self.metadata(), f, "channel::recv()").
            unwrap_or_else(|e| self.handle_desync(e, true, || self.receiver.recv()))
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let f = || self.receiver.try_recv();
        self.rr_recv(self.metadata(), f, "channel::try_recv()").
            unwrap_or_else(|e| self.handle_desync(e, false, f))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let f = || self.receiver.recv_timeout(timeout);
        self.rr_recv(self.metadata(), f, "channel::rect_timeout()").
            unwrap_or_else(|e| self.handle_desync(e, false, f))
    }

    pub(crate) fn metadata(&self) -> &RecordMetadata {
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

    pub(crate) fn flavor(&self) -> ChannelLabel {
        self.metadata.flavor
    }
}

pub fn after(duration: Duration) -> Receiver<Instant> {
    *ENV_LOGGER;

    let id = DetChannelId::new();
    log_rr!(Info, "After channel receiver created: {:?}", id);
    Receiver::new(ChannelVariant::After(crossbeam_channel::after(duration)), id)
}

pub fn bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::bounded(cap);
    let id = DetChannelId::new();
    let type_name = get_generic_name::<T>();

    log_rr!(Info, "Bounded channel created: {:?} {:?}", id, type_name);
    (
        Sender {
            sender,
            mode: *RECORD_MODE,
            channel_type: ChannelLabel::Bounded,
            channel_id: id.clone(),
            type_name: type_name.to_string(),
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
        metadata: RecordMetadata {
            type_name,
            flavor: ChannelLabel::Never,
            mode: *RECORD_MODE,
            id,
        },
    }
}
