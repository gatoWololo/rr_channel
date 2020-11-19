//! Different Channel types share the same general method of doing RR through our
//! per-channel buffers. This module encapsulates that logic via the

use crate::error;
use crate::rr;
use crate::RRMode;

use crate::detthread::{self, DetThreadId};
use crate::error::DesyncError;
use crate::{desync, recordlog};
use crate::{BufferedValues, DetMessage};

use crate::recordlog::Recordable;
use log::Level::*;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Channel themselves are assigned a deterministic ID as metadata. This make it
/// possible to ensure the message was sent from the same sender every time.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct DetChannelId {
    det_thread_id: DetThreadId,
    channel_id: u32,
}

impl DetChannelId {
    /// Create a DetChannelId using context's det_id() and channel_id()
    /// Assigns a unique id to this channel.
    pub fn new() -> DetChannelId {
        DetChannelId {
            det_thread_id: detthread::get_det_id(),
            channel_id: detthread::get_and_inc_channel_id(),
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

impl Default for DetChannelId {
    fn default() -> DetChannelId {
        DetChannelId::new()
    }
}
pub(crate) trait RecvRecordReplay<T, E> {
    /// Given a channel receiver function as a closure (e.g. || receiver.try_receive())
    /// handle the recording or replaying logic for this message arrival.
    fn record_replay_with(
        &self,
        metadata: &recordlog::RecordMetadata,
        recv_message: impl FnOnce() -> Result<DetMessage<T>, E>,
        // Ideally recordlog should be a mutable reference because, it is. But this clashes with the
        // the channel library APIs which for some reason send and recv are not mutable. So to keep
        // things simple here we also say the self is not mutable :grimace-emoji:
        recordlog: &dyn Recordable,
    ) -> desync::Result<Result<T, E>> {
        // crate::log_rr!(Debug, "Receiver<{:?}>::{}", metadata.id, function_name);

        match metadata.mode {
            RRMode::Record => {
                let (recorded, result) = match recv_message() {
                    Ok((sender_thread, msg)) => (Self::recorded_event_succ(sender_thread), Ok(msg)),
                    Err(e) => (Self::recorded_event_err(&e), Err(e)),
                };

                recordlog.write_event_to_record(recorded, &metadata);
                Ok(result)
            }
            RRMode::Replay => {
                let det_id = detthread::get_det_id();
                let entry = recordlog.get_log_entry_with(
                    det_id,
                    detthread::get_event_id(),
                    &metadata.flavor,
                    &metadata.id,
                );

                // Special case for NoEntryInLog. Hang this thread forever.
                if let Err(e @ error::DesyncError::NoEntryInLog(_, _)) = entry {
                    crate::log_rr!(Info, "Saw {:?}. Putting thread to sleep.", e);
                    desync::sleep_until_desync();
                    // Thread woke back up... desynced!
                    return Err(error::DesyncError::DesynchronizedWakeup);
                }

                Ok(Self::replay_recorded_event(self, entry?)?)
            }
            RRMode::NoRR => Ok(recv_message().map(|v| v.1)),
        }
    }

    /// Given the determininistic thread id return the corresponding successful case
    /// of RecordedEvent for record-logging.
    fn recorded_event_succ(dtid: DetThreadId) -> recordlog::RecordedEvent;

    /// Given the channel recieve error, return the corresponding successful case
    /// of RecordedEvent for record-logging. We take a reference here as there is no
    /// guarantee that our `E` implements Copy or Clone (like is the case for Ipc Error).
    fn recorded_event_err(e: &E) -> recordlog::RecordedEvent;

    /// Produce the correct value based on the logged event from the recorded execution.
    /// Returns the results of replaying the event: Result<T, E> or a DesyncError if we're
    /// unable to replay the event.
    fn replay_recorded_event(
        &self,
        event: recordlog::RecordedEvent,
    ) -> desync::Result<Result<T, E>>;
}

/// Abstract over logic to send a message while recording or replaying results.
pub(crate) trait SendRecordReplay<T, E> {
    /// RecordedEvent variant for this type.
    const EVENT_VARIANT: recordlog::RecordedEvent;

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
        metadata: &recordlog::RecordMetadata,
        recordlog: &dyn Recordable,
        // TODO Use tracing to give context instead of passing unused parameter.
        // sender_name: &str,
    ) -> Result<Result<(), E>, (DesyncError, T)> {
        if desync::program_desyned() {
            return Err((error::DesyncError::Desynchronized, msg));
        }

        // However, for the record log, we still want to use the original
        // thread's DetThreadId. Otherwise we will have "repeated" entries in the log
        // which look like they're coming from the same thread.
        let forwading_id = detthread::get_forwarding_id();
        // crate::log_rr!(
        //     Info,
        //     "{}<{:?}>::send(({:?}, {:?}))",
        //     sender_name,
        //     metadata.id,
        //     forwading_id,
        //     metadata.type_name
        // );

        match metadata.mode {
            RRMode::Record => {
                // Note: send() must come before rr::log() as it internally increments
                // event_id.
                let result = self.underlying_send(forwading_id, msg);
                recordlog.write_event_to_record(Self::EVENT_VARIANT, &metadata);
                Ok(result)
            }
            RRMode::Replay => {
                let det_id = detthread::get_det_id();
                // Ugh. This is ugly. I need it though. As this function moves the `T`.
                // If we encounter an error we need to return the `T` back up to the caller.
                // crossbeam_channel::send() does pretty much the same thing.
                match recordlog.get_log_entry_with(
                    det_id,
                    detthread::get_event_id(),
                    &metadata.flavor,
                    &metadata.id,
                ) {
                    // Special case for NoEntryInLog. Hang this thread forever.
                    Err(e @ error::DesyncError::NoEntryInLog(_, _)) => {
                        crate::log_rr!(Info, "Saw {:?}. Putting thread to sleep.", e);
                        desync::sleep_until_desync();
                        // Thread woke back up... desynced!
                        return Err((error::DesyncError::DesynchronizedWakeup, msg));
                    }
                    // TODO: Hmmm when does this error case happen?
                    Err(e) => return Err((e, msg)),
                    Ok(event) => {
                        if let Err(e) = rr::SendRecordReplay::check_log_entry(self, event) {
                            return Err((e, msg));
                        }
                    }
                }
                let result = rr::SendRecordReplay::underlying_send(self, forwading_id, msg);
                detthread::inc_event_id();
                Ok(result)
            }
            RRMode::NoRR => Ok(self.underlying_send(detthread::get_det_id(), msg)),
        }
    }

    /// Given the current entry in the log checks that the expected `RecordedEvent` variant is
    /// present.
    fn check_log_entry(&self, entry: recordlog::RecordedEvent) -> desync::Result<()>;

    /// Call underlying channel's send function to send this message and thread_id to receiver.
    fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), E>;
}

/// Reads messages from `receiver_with_timeout` waiting for message to arrive from `expected_sender`.
/// This is a generic function used by different channel implementations for receiving the
/// correct value.
pub(crate) fn recv_expected_message<T>(
    expected_sender: &DetThreadId,
    receiver_with_timeout: impl Fn() -> Result<DetMessage<T>, error::RecvErrorRR>,
    buffer: &mut BufferedValues<T>,
    // id: &DetChannelId,
) -> desync::Result<T> {
    // crate::log_rr!(
    //     Debug,
    //     "recv_from_sender(sender: {:?}, id: {:?}) ...",
    //     expected_sender,
    //     id
    // );

    // Check our buffer to see if this value is already here.
    if let Some(val) = buffer.get_mut(expected_sender).and_then(|q| q.pop_front()) {
        // crate::log_rr!(
        //     Debug,
        //     "replay_recv(): Recv message found in buffer. Channel id: {:?}",
        //     id
        // );
        return Ok(val);
    }

    // Loop until we get the message we're waiting for. All "wrong" messages are
    // buffered into self.buffer.
    loop {
        let (msg_sender, msg) = receiver_with_timeout()?;
        if msg_sender == *expected_sender {
            crate::log_rr!(Debug, "Recv message found through recv()");
            return Ok(msg);
        } else {
            // Value did no match. Buffer it. Handles both `none_buffer` and
            // regular `buffer` case.
            // crate::log_rr!(Debug, "Wrong value found, buffering it for: {:?}", id);
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
    use crate::{init_tivo_thread_root, BufferedValues};
    use std::borrow::Borrow;
    use std::collections::VecDeque;

    #[test]
    fn det_chan_id() {
        init_tivo_thread_root();
        let chanid1 = DetChannelId::new();
        let chanid2 = DetChannelId::new();

        assert_eq!(chanid1.channel_id, 1);
        assert_eq!(chanid2.channel_id, 2);
        assert_eq!(chanid1.det_thread_id, chanid2.det_thread_id);
    }

    #[test]
    fn det_chan_id2() {
        init_tivo_thread_root();
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
