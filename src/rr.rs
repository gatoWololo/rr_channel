//! Different Channel types share the same general method of doing RR through our
//! per-channel buffers. This module encapsulates that logic via the
// use crate::DetThreadId;
use crate::RRMode;
use crate::error;
use crate::rr;

use crate::{DesyncMode, DESYNC_MODE};
use crate::detthread::{self, DetThreadId};
use crate::{recordlog, desync};
use crate::error::DesyncError;

use log::Level::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use std::cell::RefMut;
use std::collections::VecDeque;

/// Channel themselves are assigned a deterministic ID as metadata. This make it
/// possible to ensure the message was sent from the same sender every time.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct DetChannelId {
    det_thread_id: Option<DetThreadId>,
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

    /// Sometimes we need a DetChannelId to fulfill an API, but it won't
    /// be used at all. Create a fake one here. Later we might get rid of
    /// this an use a Option instead...
    pub fn fake() -> DetChannelId {
        DetChannelId {
            det_thread_id: None,
            channel_id: 0,
        }
    }
}

use std::error::Error;

pub trait RecvRR<T, E: Error> {
    fn rr_recv(
        &self,
        metadata: &recordlog::RecordMetadata,
        recv: impl FnOnce() -> Result<(Option<DetThreadId>, T), E>,
        function_name: &str,
    ) -> Result<Result<T, E>, DesyncError> {
        crate::log_rr!(Debug, "Receiver<{:?}>::{}", metadata.id, function_name);

        match metadata.mode {
            RRMode::Record => {
                let (result, recorded) = self.to_recorded_event(recv());
                recordlog::log(recorded, metadata.flavor, &metadata.type_name, &metadata.id);
                Ok(result)
            }
            RRMode::Replay => {
                match detthread::get_det_id() {
                    None => {
                        crate::log_rr!(
                            Warn,
                            "DetThreadId was None. \
                                       This execution may not be deterministic."
                        );
                        detthread::inc_event_id();
                        // TODO: This seems wrong. I think we should be doing
                        // a replay_recv() here even if the det_id is none.
                        Ok(recv().map(|v| v.1))
                    }
                    Some(det_id) => {
                        let entry = recordlog::get_log_entry_with(
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

                        Ok(Self::expected_recorded_events(self, entry?.clone())?)
                    }
                }
            }
            RRMode::NoRR => Ok(recv().map(|v| v.1)),
        }
    }

    /// Handles desynchronization events based on global DESYNC_MODE.
    fn handle_desync(
        &self,
        desync: DesyncError,
        can_block: bool,
        do_recv: impl Fn() -> Result<(Option<DetThreadId>, T), E>,
    ) -> Result<T, E> {
        crate::log_rr!(Warn, "Desynchonization found: {:?}", desync);

        match *DESYNC_MODE {
            DesyncMode::KeepGoing => {
                desync::mark_program_as_desynced();
                detthread::inc_event_id();
                self.desync_get_next_entry(&do_recv)
            }
            DesyncMode::Panic => {
                panic!("Desynchonization found: {:?}", desync);
            }
        }
    }

    fn desync_get_next_entry(
        &self,
        do_recv: &impl Fn() -> Result<(Option<DetThreadId>, T), E>,
    ) -> Result<T, E> {
        // Try using entry from buffer before recv()-ing directly from
        // receiver. We don't care who the expected sender was. Any
        // value will do.
        for queue in self.get_buffer().values_mut() {
            if let Some(val) = queue.pop_front() {
                return Ok(val);
            }
        }
        // No entries in buffer. Read from the wire.
        do_recv().map(|t| t.1)
    }

    fn get_buffer(&self) -> RefMut<HashMap<Option<DetThreadId>, VecDeque<T>>>;

    fn to_recorded_event(
        &self,
        event: Result<(Option<DetThreadId>, T), E>,
    ) -> (Result<T, E>, recordlog::RecordedEvent);

    fn expected_recorded_events(&self, event: recordlog::RecordedEvent) -> Result<Result<T, E>, DesyncError>;
}

/// Abstract over logic to send a message while recording or replaying results.
pub(crate) trait SendRR<T, E> {
    /// RecordedEvent variant for this type.
    const EVENT_VARIANT: recordlog::RecordedEvent;

    /// Attempts to send message to receiver. Handles forwading the ID for router.
    /// On Record: Sends message and records result in log.
    /// On Replay: Checks correct message is sent and handles desynchonization errors.
    /// Two different things can fail here:
    /// 1) We could be in a desync state. This is the "outer" result which returns the original
    ///    `msg: T` value.
    /// 2) The recursive call the the channel send method for this type of channel. This is the inner
    ///    result.
    fn rr_send(
        &self,
        msg: T,
        mode: &RRMode,
        id: &DetChannelId,
        type_name: &str,
        flavor: &recordlog::ChannelLabel,
        sender_name: &str,
    ) -> Result<Result<(), E>, (DesyncError, T)> {
        if desync::program_desyned() {
            return Err((error::DesyncError::Desynchronized, msg));
        }

        // However, for the record log, we still want to use the original
        // thread's DetThreadId. Otherwise we will have "repeated" entries in the log
        // which look like they're coming from the same thread.
        let forwading_id = get_forward_id();
        crate::log_rr!(
            Info,
            "{}<{:?}>::send(({:?}, {:?}))",
            sender_name,
            id,
            forwading_id,
            type_name
        );

        match mode {
            RRMode::Record => {
                // Note: send() must come before rr::log() as it internally
                // increments event_id.
                let result = self.send(forwading_id, msg);
                recordlog::log(Self::EVENT_VARIANT, flavor.clone(), type_name, id);
                Ok(result)
            }
            RRMode::Replay => {
                if let Some(det_id) = detthread::get_det_id() {
                    // Ugh. This is ugly. I need it though. As this function moves the `T`.
                    // If we encounter an error we need to return the `T` back up to the caller.
                    // crossbeam_channel::send() does pretty much the same thing.
                    match recordlog::get_log_entry_with(det_id, detthread::get_event_id(), flavor, id) {
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
                            if let Err(e) = rr::SendRR::check_log_entry(self, event.clone()) {
                                return Err((e, msg));
                            }
                        }
                    }
                } else {
                    crate::log_rr!(
                        Warn,
                        "det_id is None. This execution may be nondeterministic"
                    );
                }

                let result = rr::SendRR::send(self, forwading_id, msg);
                detthread::inc_event_id();
                Ok(result)
            }
            RRMode::NoRR => Ok(self.send(detthread::get_det_id(), msg)),
        }
    }

    /// Given the current entry in the log checks that the expected `RecordedEvent` variant is
    /// present.
    fn check_log_entry(&self, entry: recordlog::RecordedEvent) -> Result<(), DesyncError>;

    /// Call underlying channel's send function to send this message and thread_id to receiver.
    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), E>;
}

/// Generic function to loop on differnt kinds of replays: recv(), try_recv(), etc.
/// and buffer wrong entries.
pub(crate) fn recv_from_sender<T>(
    expected_sender: &Option<DetThreadId>,
    rr_timeout_recv: impl Fn() -> Result<(Option<DetThreadId>, T), error::RecvErrorRR>,
    buffer: &mut RefMut<HashMap<Option<DetThreadId>, VecDeque<T>>>,
    id: &DetChannelId,
) -> Result<T, DesyncError> {
    crate::log_rr!(
        Debug,
        "recv_from_sender(sender: {:?}, id: {:?}) ...",
        expected_sender,
        id
    );

    // Check our buffer to see if this value is already here.
    if let Some(val) = buffer.get_mut(expected_sender).and_then(|e| e.pop_front()) {
        crate::log_rr!(
            Debug,
            "replay_recv(): Recv message found in buffer. Channel id: {:?}",
            id
        );
        return Ok(val);
    }

    // Loop until we get the message we're waiting for. All "wrong" messages are
    // buffered into self.buffer.
    loop {
        let (msg_sender, msg) = rr_timeout_recv()?;
        if msg_sender == *expected_sender {
            crate::log_rr!(Debug, "Recv message found through recv()");
            return Ok(msg);
        } else {
            // Value did no match. Buffer it. Handles both `none_buffer` and
            // regular `buffer` case.
            crate::log_rr!(Debug, "Wrong value found. Queing it for: {:?}", id);
            buffer
                .entry(msg_sender)
                .or_insert(VecDeque::new())
                .push_back(msg);
        }
    }
}

/// Because of the router, we want to "forward" the original sender's DetThreadId
/// sometimes.
pub fn get_forward_id() -> Option<DetThreadId> {
    if detthread::in_forwarding() {
        detthread::get_temp_det_id()
    } else {
        detthread::get_det_id()
    }
}
