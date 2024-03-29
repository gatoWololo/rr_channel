//! Different Channel types share the same general method of doing RR through our
//! per-channel buffers. This module encapsulates that logic via the

use crate::EventRecorder;
use crate::RRMode;

use crate::detthread::{self, get_det_id, DetThreadId, CHANNEL_ID};
use crate::error::{DesyncError, RecvErrorRR};
use crate::{desync, recordlog};
use crate::{BufferedValues, DetMessage};

use crate::recordlog::{RecordEntry, RecordMetadata, TivoEvent};
use core::any::type_name;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt::{Display, Formatter};
use std::sync::atomic::Ordering;

#[allow(unused_imports)]
use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

/// Channel themselves are assigned a deterministic ID as metadata. This make it
/// possible to ensure the message was sent from the same sender every time.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct DetChannelId {
    pub(crate) det_thread_id: DetThreadId,
    pub(crate) channel_id: u32,
}

/// Used for better tracing logging output.
impl Display for DetChannelId {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "DetChannelId{{{:?}, {:?}}}",
            self.det_thread_id, self.channel_id
        )
    }
}

impl DetChannelId {
    /// Create a DetChannelId using context's det_id() and channel_id()
    /// Assigns a unique id to this channel.
    fn new() -> DetChannelId {
        let channel_id = CHANNEL_ID.with(|ci| ci.fetch_add(1, Ordering::SeqCst));
        debug!("New Channel id generated for ID {:?}", channel_id);
        DetChannelId {
            det_thread_id: detthread::get_det_id(),
            channel_id,
        }
    }

    /// It is not obvious that DetChannelId::new() changes the global state by incrementing CHANNEL_ID.
    /// this bit me before. Only allow DetChannelId generation via this function to make it more
    /// explicit what is happening.
    pub(crate) fn generate_new_unique_channel_id() -> DetChannelId {
        DetChannelId::new()
    }

    #[cfg(test)]
    pub(crate) fn from_raw(dti: DetThreadId, channel_id: u32) -> DetChannelId {
        DetChannelId {
            det_thread_id: dti,
            channel_id,
        }
    }

    /// Sometimes we need a DetChannelId to fulfill an API, but it won't be used at all.
    /// Create a fake one here. Later we might get rid of this an use a Option instead...
    pub fn fake() -> DetChannelId {
        DetChannelId {
            // TODO: Is there a better value to show this is a mock DTI?
            det_thread_id: DetThreadId::new(),
            channel_id: 0,
        }
    }
}

/// The E type parameter is not used in the API of this trait, we use E to allow the same type,
/// say crossbeam-channel::Receiver to implement this trait for multiple different `E`s. For example,
/// for RecordEventChecker<TryRecvErr>, RecordEventChecker<TimeoutRecvErr>, etc. This is due to
/// our RecordEventChecker relating to TivoEvents, there may be multiple TivoEvents related to a
/// specific Rust type, but traits only work on types.
pub(crate) trait RecordEventChecker<E> {
    /// Returns Ok if the passed event matches the expected event for Self. Otherwise returns
    /// Err of the expected event.
    fn check_recorded_event(&self, re: &TivoEvent) -> Result<(), TivoEvent>;

    /// Compare the expected values of the RecordEntry against the real values we're seeing. Some
    /// commands have significant error checking that makes the meaning of the code hard to
    /// understand. So we check it here.
    fn check_event_mismatch(
        &self,
        record_entry: &RecordEntry,
        metadata: &RecordMetadata,
    ) -> desync::Result<()> {
        if let Err(expected_event) = self.check_recorded_event(&record_entry.event) {
            let e = DesyncError::EventMismatch(record_entry.event.clone(), expected_event);
            error!(%e);
            return Err(e);
        }

        record_entry.check_mismatch(metadata)?;
        Ok(())
    }
}

pub(crate) trait RecvRecordReplay<T, E>: RecordEventChecker<E> {
    /// Given a channel receiver function as a closure (e.g. || receiver.try_receive())
    /// handle the recording or replaying logic for this message arrival.
    fn record_replay_recv(
        &self,
        mode: &RRMode,
        metadata: &recordlog::RecordMetadata,
        recv_message: impl FnOnce() -> Result<DetMessage<T>, E>,
        recordlog: &EventRecorder,
    ) -> desync::Result<Result<T, E>> {
        match mode {
            RRMode::Record => {
                let (recorded, result) = match recv_message() {
                    Ok((sender_thread, msg)) => (Self::recorded_event_succ(sender_thread), Ok(msg)),
                    Err(e) => (Self::recorded_event_err(&e), Err(e)),
                };

                recordlog.write_event_to_record(recorded, &metadata)?;
                Ok(result)
            }
            RRMode::Replay => {
                let record_entry = recordlog.get_log_entry();

                // Special case for NoEntryInLog. Hang this thread forever.
                if let Err(e @ DesyncError::NoEntryInLog) = record_entry {
                    info!("Saw {:?}. Putting thread to sleep.", e);
                    desync::sleep_until_desync();

                    // Thread woke back up... desynced!
                    let error = DesyncError::DesynchronizedWakeup;
                    error!(%error);
                    return Err(error);
                }

                let record_entry = record_entry?;
                let type_name = record_entry.type_name.clone();
                self.check_event_mismatch(&record_entry, metadata)?;

                let received =
                    Self::replay_recorded_event(self, record_entry.event).map_err(|e| {
                        if let DesyncError::Timeout(None) = e {
                            DesyncError::Timeout(Some(type_name))
                        } else {
                            e
                        }
                    })?;
                Ok(received)
            }
            RRMode::NoRR => Ok(recv_message().map(|v| v.1)),
        }
    }

    /// Given the deterministic thread id return the corresponding successful case
    /// of TivoEvent for record-logging.
    fn recorded_event_succ(dtid: DetThreadId) -> recordlog::TivoEvent;

    /// Given the channel receive error, return the corresponding successful case
    /// of TivoEvent for record-logging. We take a reference here as there is no
    /// guarantee that our `E` implements Copy or Clone (like is the case for Ipc Error).
    fn recorded_event_err(e: &E) -> recordlog::TivoEvent;

    /// Produce the correct value based on the logged event from the recorded execution.
    /// Returns the results of replaying the event: Result<T, E> or a DesyncError if we're
    /// unable to replay the event.
    fn replay_recorded_event(&self, event: recordlog::TivoEvent) -> desync::Result<Result<T, E>>;
}

/// Abstract over logic to send a message while recording or replaying results.
pub(crate) trait SendRecordReplay<T, E>: RecordEventChecker<E> {
    /// TivoEvent variant for this type.
    const EVENT_VARIANT: recordlog::TivoEvent;

    /// In theory we shouldn't need to record send events. In practice, it is useful for debugging
    /// and for detecting when things have gone wrong.
    /// Attempts to send message to receiver. Handles forwarding the ID for router.
    /// On Record: Sends message and records result in log.
    /// On Replay: Checks correct message is sent and handles desynchonization errors.
    /// Two different things can fail here:
    /// 1) We could be in a desync state. This is the "outer" result which returns the original
    ///    `msg: T` value.
    /// 2) The recursive call the the channel send method for this type of channel. This is the inner
    ///    result.
    fn record_replay_send(
        &self,
        msg: T,
        mode: &RRMode,
        metadata: &recordlog::RecordMetadata,
        recordlog: &EventRecorder,
    ) -> Result<Result<(), E>, (DesyncError, T)> {
        if desync::program_desyned() {
            let error = DesyncError::Desynchronized;
            error!(%error);
            return Err((error, msg));
        }
        let det_id = get_det_id();
        info!("send(({:?}, {:?})", metadata.id, det_id);
        match mode {
            RRMode::Record => {
                let result = self.underlying_send(det_id, msg);
                if let Err(e) = recordlog.write_event_to_record(Self::EVENT_VARIANT, &metadata) {
                    panic!("Unable to write to log: {}", e)
                }
                    // Use expect here instead of '?' as I don't have a 'T' to return as the error
                    // type of this function expects.
                Ok(result)
            }
            RRMode::Replay => {
                // Ugh. This is ugly. I need it though. As this function moves the `T`.
                // If we encounter an error we need to return the `T` back up to the caller.
                // crossbeam_channel::send() does pretty much the same thing.

                match recordlog.get_log_entry() {
                    // Special case for NoEntryInLog. Hang this thread forever.
                    Err(e @ DesyncError::NoEntryInLog) => {
                        info!("Saw {:?}. Putting thread to sleep.", e);
                        desync::sleep_until_desync();

                        // Thread woke back up... desynced!
                        let error1 = DesyncError::DesynchronizedWakeup;
                        error!(%error1);
                        return Err((error1, msg));
                    }
                    // TODO: Hmmm when does this error case happen?
                    Err(e) => return Err((e, msg)),
                    Ok(recorded_entry) => {
                        // TODO Ugly write better.
                        if let Err(e) = self.check_event_mismatch(&recorded_entry, &metadata) {
                            return Err((e, msg));
                        }
                    }
                }
                let result = self.underlying_send(det_id, msg);
                Ok(result)
            }
            RRMode::NoRR => Ok(self.underlying_send(det_id, msg)),
        }
    }

    /// Call underlying channel's send function to send this message and thread_id to receiver.
    fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), E>;
}

/// Reads messages from `receiver_with_timeout` waiting for message to arrive from `expected_sender`.
/// This is a generic function used by different channel implementations for receiving the
/// correct value.
pub(crate) fn recv_expected_message<T>(
    expected_sender: &DetThreadId,
    receiver_with_timeout: impl Fn() -> Result<DetMessage<T>, RecvErrorRR>,
    buffer: &mut BufferedValues<T>,
) -> desync::Result<T> {
    let _s = span!(
        Level::DEBUG,
        stringify!(recv_expected_message),
        ?expected_sender,
        "type" = type_name::<T>()
    )
    .entered();
    debug!("recv_from_sender()");

    // Check our buffer to see if this value is already here.
    if let Some(val) = buffer.get_mut(expected_sender).and_then(|q| q.pop_front()) {
        debug!("Recv message found in buffer.");
        return Ok(val);
    }

    // Loop until we get the message we're waiting for. All "wrong" messages are
    // buffered into self.buffer.
    loop {
        let (msg_sender, msg) = match receiver_with_timeout() {
            Ok(k) => k,
            e @ Err(_) => {
                error!(
                    "{:?} Timed out while wait for message from: {:?}.",
                    get_det_id(),
                    expected_sender,
                );

                let other_thread_msgs = buffer.len();
                let other_keys = buffer.keys();
                error!(
                    "{} entries found buffered from other threads: {:?}",
                    other_thread_msgs, other_keys,
                );
                e?
            }
        };
        if msg_sender == *expected_sender {
            debug!("Recv message found through recv()");
            return Ok(msg);
        } else {
            // Value did no match. Buffer it. Handles both `none_buffer` and
            // regular `buffer` case.
            trace!("Message arrived out-of-order from: {:?}", msg_sender);
            buffer
                .entry(msg_sender)
                .or_insert_with(VecDeque::new)
                .push_back(msg);
        }
    }
}

#[cfg(test)]
mod test {
    use crate::detthread::{spawn, DetThreadId};
    use crate::rr::{recv_expected_message, DetChannelId};
    use crate::test::set_rr_mode;
    use crate::Tivo;
    use crate::{BufferedValues, RRMode};
    use std::borrow::Borrow;
    use std::collections::VecDeque;

    #[test]
    fn det_chan_id() {
        Tivo::init_tivo_thread_root_test();
        let chanid1 = DetChannelId::new();
        let chanid2 = DetChannelId::new();

        assert_eq!(chanid1.channel_id, 1);
        assert_eq!(chanid2.channel_id, 2);
        assert_eq!(chanid1.det_thread_id, chanid2.det_thread_id);
    }

    #[test]
    fn det_chan_id2() {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);

        let c1 = DetChannelId::new();
        spawn(move || {
            let c2 = DetChannelId::new();
            // First chanid in this thread. Should have ID of one not two.
            assert_eq!(c2.channel_id, 1);
            assert_ne!(c1.det_thread_id, c2.det_thread_id);
        });
    }

    // Message already in buffer.
    #[test]
    fn recv_expected_message_value_in_buffer() {
        let val = 10;
        let mut bv = BufferedValues::new();
        let det_id = DetThreadId::from([1].borrow());
        let mut queue = VecDeque::new();
        queue.push_back(val);

        bv.insert(DetThreadId::from([1].borrow()), queue);
        let f = || panic!("Value was in buffer, this fn should not have been called.");

        let v = recv_expected_message(&det_id, f, &mut bv).expect("Value should have been found");
        assert_eq!(v, val);
        // Check queue is empty.
        assert!(bv.get(&det_id).unwrap().is_empty());
    }

    // Empty buffer. Closure will be polled for message where it will be found.
    #[test]
    fn recv_expected_message_empty_buffer() {
        let val = 10;
        let mut bv = BufferedValues::new();
        let det_id = DetThreadId::from([1].borrow());
        let f = || Ok((det_id.clone(), val));

        let v = recv_expected_message(&det_id, f, &mut bv).expect("Value should have been found");
        assert_eq!(v, val);
        // Check queue is empty.
        assert!(bv.is_empty());
    }

    // Message not in buffer. Exercises loop in recv_expected_messages.
    #[test]
    fn recv_expected_message_multi_poll() {
        // Pretend this channel represents det threads sending messages.
        let (s, r) = crossbeam_channel::unbounded::<(DetThreadId, u32)>();
        let sender1 = DetThreadId::from([1].borrow());
        let sender2 = DetThreadId::from([2].borrow());

        s.send((sender1.clone(), 10)).expect("failed to send");
        s.send((sender2.clone(), 11)).expect("failed to send");

        let mut bv = BufferedValues::new();
        let f = move || Ok(r.recv().unwrap());

        let v = recv_expected_message(&sender2, f, &mut bv).expect("Value should have been found");
        // Found correct value.
        assert_eq!(v, 11);
        // First value taken off channel and placed in queue.
        assert_eq!(10, bv.get_mut(&sender1).unwrap().pop_back().unwrap());
    }
}

// #[cfg(test)]
// mod test {
//     trait Receiver {
//         fn recv(&self)
//     }
//
// }
