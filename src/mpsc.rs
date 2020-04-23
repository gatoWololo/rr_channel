use std::sync::mpsc;
// use std::sync::mpsc::{RecvError, SendError, RecvTimeoutError, TryRecvError};
use crate::get_generic_name;
use crate::record_replay::recv_from_sender;
use crate::record_replay::{self, Blocking, FlavorMarker, Recorded,
                           RecordMetadata, RecordReplayRecv, RecordReplaySend,
                           DetChannelId, DesyncError, get_forward_id,
                           RecvErrorRR};
use crate::thread::get_and_inc_channel_id;
use crate::thread::*;
use crate::{RecordReplayMode, ENV_LOGGER, RECORD_MODE, DesyncMode, DESYNC_MODE};
use log::Level::*;
use crate::record_replay::mark_program_as_desynced;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::error::Error;
use std::time::Duration;
use std::time::Instant;
use std::cell::RefMut;
use crate::log_rr;


#[derive(Debug)]
pub struct MpscReceiver<T> {
    pub(crate) receiver: ReceiverFlavor<T>,
    pub(crate) metadata: RecordMetadata,
    pub(crate) buffer: RefCell<HashMap<Option<DetThreadId>, VecDeque<T>>>,
}

#[derive(Debug)]
pub struct MpscSender<T> {
    pub(crate) sender: SenderFlavor<T>,
    pub(crate) metadata: RecordMetadata,
}

#[derive(Debug)]
pub enum ReceiverFlavor<T> {
    Bounded(mpsc::Receiver<(Option<DetThreadId>, T)>,),
    Unbounded(mpsc::Receiver<(Option<DetThreadId>, T)>,),
}

#[derive(Debug, Clone)]
pub enum SenderFlavor<T> {
    Bounded(mpsc::SyncSender<(Option<DetThreadId>, T)>),
    Unbounded(mpsc::Sender<(Option<DetThreadId>, T)>),
}

macro_rules! impl_RecordReplay {
    ($err_type:ty, $succ: ident, $err:ident) => {
        impl<T> RecordReplayRecv<T, $err_type> for MpscReceiver<T> {
            fn to_recorded_event(
                &self,
                event: Result<(Option<DetThreadId>, T), $err_type>,
            ) -> (Result<T, $err_type>, Recorded) {
                match event {
                    Ok((sender_thread, msg)) => {
                        (Ok(msg), Recorded::$succ { sender_thread })
                    }
                    Err(e) => {
                        (Err(e), Recorded::$err(e))
                    }
                }
            }

            fn expected_recorded_events(&self, event: Recorded)
                                        -> Result<Result<T, $err_type>, DesyncError> {
                match event {
                    Recorded::$succ { sender_thread } => {
                        let retval = self.replay_recv(&sender_thread)?;
                        // Here is where we explictly increment our event_id!
                        inc_event_id();
                        Ok(Ok(retval))
                    }
                    Recorded::$err(e) => {
                        log_rr!(Trace, "Creating error event for: {:?}", Recorded::$err(e));
                        // Here is where we explictly increment our event_id!
                        inc_event_id();
                        Ok(Err(e))
                    }
                    e => {
                        let mock_event = Recorded::$succ { sender_thread: None };
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

impl_RecordReplay!(mpsc::RecvError, MpscRecvSucc, MpscRecvErr);
impl_RecordReplay!(mpsc::TryRecvError, MpscTryRecvSucc, MpscTryRecvErr);
impl_RecordReplay!(mpsc::RecvTimeoutError, MpscRecvTimeoutSucc, MpscRecvTimeoutErr);

impl<T> MpscReceiver<T> {
    pub fn new(real_receiver: ReceiverFlavor<T>, id: DetChannelId) -> MpscReceiver<T> {
        let flavor = MpscReceiver::get_marker(&real_receiver);
        MpscReceiver {
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

    /// Poll to see if receiver has this entry. Polling is side-effect-y and
    /// takes the message off the channel. The message is sent to the internal
    /// buffer to be retreived later.
    /// Notice even on `false`, an arbitrary number of messages from _other_ senders
    /// may be buffered.
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

    /// This is the raw function called in a loop by our record_replay trait.
    /// It should not be called directly by anyone, as there is buffering
    /// that will not be properly taken into account if called directly.
    fn rr_recv_timeout(&self, timeout: Duration)
                       -> Result<(Option<DetThreadId>, T), RecvErrorRR> {
        self.receiver.recv_timeout(timeout).
            map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => RecvErrorRR::Timeout,
                mpsc::RecvTimeoutError::Disconnected => RecvErrorRR::Disconnected,
            })
    }

    pub(crate) fn replay_recv(&self, sender: &Option<DetThreadId>) -> Result<T, DesyncError> {
        self.replay_recv_timeout(sender, Duration::from_secs(1))
    }

    pub(crate) fn replay_recv_timeout(&self, sender: &Option<DetThreadId>, timeout: Duration)
                                 -> Result<T, DesyncError> {
        recv_from_sender(
            &sender,
            || self.rr_recv_timeout(timeout),
            &mut self.buffer.borrow_mut(),
            &self.metadata.id,
        )
    }

    pub(crate) fn flavor(&self) -> FlavorMarker {
        self.metadata.flavor
    }

    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        let f = || self.receiver.recv();
        self.record_replay_recv(self.metadata(), f, "channel::recv()").
            unwrap_or_else(|e| self.handle_desync(e, true, || self.receiver.recv()))
    }

    pub fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        let f = || self.receiver.try_recv();
        self.record_replay_recv(self.metadata(), f, "channel::try_recv()").
            unwrap_or_else(|e| self.handle_desync(e, false, f))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        let f = || self.receiver.recv_timeout(timeout);
        self.record_replay_recv(self.metadata(), f, "channel::recv_timeout()").
            unwrap_or_else(|e| self.handle_desync(e, false, f))
    }

    pub fn metadata(&self) -> &RecordMetadata {
        &self.metadata
    }

    pub fn get_marker(receiver: &ReceiverFlavor<T>) -> FlavorMarker {
        match receiver {
            ReceiverFlavor::Unbounded(_) => FlavorMarker::MpscUnbounded,
            ReceiverFlavor::Bounded(_) => FlavorMarker::MpscBounded,
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
    }
}

impl<T> ReceiverFlavor<T> {
    generate_receiver_method!(recv, mpsc::RecvError);
    generate_receiver_method!(try_recv, mpsc::TryRecvError);

    pub fn recv_timeout(
        &self,
        duration: Duration,
    ) -> Result<(Option<DetThreadId>, T), mpsc::RecvTimeoutError> {
        // TODO
        match self {
            ReceiverFlavor::Bounded(receiver) |
            ReceiverFlavor::Unbounded(receiver) => match receiver.recv_timeout(duration) {
                Ok(msg) => Ok(msg),
                e => e,
            },
            _ => unreachable!(),
        }
    }
}

// TODO
impl<T> MpscSender<T> {
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, msg: T) -> Result<(), mpsc::SendError<T>> {
        match self.record_replay_send(
            msg,
            &self.metadata.mode,
            &self.metadata.id,
            &self.metadata.type_name,
            &self.metadata.flavor,
            "MpscSender",
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
                        let res = RecordReplaySend::send(self, get_forward_id(), msg);
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

impl<T> Clone for MpscSender<T> where T: Clone {
    fn clone(&self) -> Self {
        // following implementation for crossbeam, we do not support MPSC for bounded channels as the blocking semantics are
        // more complicated to implement.
        if self.metadata.flavor == FlavorMarker::MpscBounded {
            log_rr!(Warn,
                    "MPSC for bounded channels not supported. Blocking semantics \
                     of bounded channels will not be preseved!");
        }
        MpscSender {
            sender: self.sender.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

impl<T> RecordReplaySend<T, mpsc::SendError<T>> for MpscSender<T> {
    fn check_log_entry(&self, entry: Recorded) -> Result<(), DesyncError> {
        match entry {
            Recorded::MpscSender => Ok(()),
            log_event => Err(DesyncError::EventMismatch(log_event, Recorded::MpscSender)),
        }
    }

    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), mpsc::SendError<T>> {
        self.sender.send((thread_id, msg))
    }

    fn as_recorded_event(&self) -> Recorded {
        Recorded::MpscSender
    }
}

macro_rules! generate_try_send {
    () => {
        // if let SenderFlavor::Bounded(sender) = self {
        //     pub fn try_send(&self, t: (Option<DetThreadId>, T)) -> Result<(), mpsc::TrySendError<T>> {
        //         sender.try_send(t);
        //     }
        // }
    }
}

impl<T> SenderFlavor<T> {
    generate_try_send!();

    pub fn send(&self, t: (Option<DetThreadId>, T)) -> Result<(), mpsc::SendError<T>> {
        unimplemented!();

        // match self {
        //     Self::Bounded(sender) => {
        //         sender.send(t).map_err(|e| mpsc::SendError(t.1))
        //     },
        //     Self::Unbounded(sender) => {
        //         sender.send(t).map_err(|e| mpsc::SendError(t.1))
        //     }
        // }
    }
}

// impl<T> Clone for SenderFlavor<T> {
//     fn clone(&self) -> Self {
//         // following implementation for crossbeam, we do not support MPSC for bounded channels as the blocking semantics are
//         // more complicated to implement.
//         if self.metadata.flavor == FlavorMarker::MpscBounded {
//             log_rr!(Warn,
//                     "MPSC for bounded channels not supported. Blocking semantics \
//                      of bounded channels will not be preseved!");
//         }
//         MpscSender {
//             sender: self.sender.clone(),
//             metadata: self.metadata.clone(),
//         }
//     }
// }

pub fn sync_channel<T>(bound: usize) -> (MpscSender<T>, MpscReceiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = mpsc::sync_channel(bound);
    let mode = *RECORD_MODE;
    let channel_type = FlavorMarker::MpscBounded;
    let type_name = get_generic_name::<T>();
    let id = DetChannelId::new();

    log_rr!(Info, "Bounded mpsc channel created: {:?} {:?}", id, type_name);
    let metadata = RecordMetadata {
        type_name: type_name.to_string(),
        flavor: channel_type,
        mode,
        id: id.clone(),           
    };
    (

        MpscSender {
            sender: SenderFlavor::Bounded(sender),
            metadata: metadata.clone()
        },

        MpscReceiver::new(ReceiverFlavor::Bounded(receiver), id)
    )
}

pub fn channel<T>() -> (MpscSender<T>, MpscReceiver<T>) {
    *ENV_LOGGER;

    let (sender, receiver) = mpsc::channel();
    let mode = *RECORD_MODE;
    let channel_type = FlavorMarker::MpscUnbounded;
    let type_name = get_generic_name::<T>();
    let id = DetChannelId::new();

    log_rr!(Info, "Unbounded mpsc channel created: {:?} {:?}", id, type_name);
    let metadata = RecordMetadata {
        type_name: type_name.to_string(),
        flavor: channel_type,
        mode,
        id: id.clone(),           
    };
    (
        

        MpscSender {
            sender: SenderFlavor::Unbounded(sender),
            metadata: metadata.clone()
        },

        MpscReceiver::new(ReceiverFlavor::Unbounded(receiver), id)
    )
}