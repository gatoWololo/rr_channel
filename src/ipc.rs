use crate::record_replay::mark_program_as_desynced;
use crate::record_replay::program_desyned;
use crate::record_replay::{self, Blocking, FlavorMarker, IpcDummyError,
                           RecordReplayRecv, Recorded, RecordMetadata,
                           get_log_entry, DetChannelId, IpcSelectEvent,
                           RecordReplaySend, get_log_entry_with, DesyncError,
                           get_forward_id, BlockingOp };
use crate::thread::{get_and_inc_channel_id, get_det_id, DetThreadId,
                    get_event_id, inc_event_id, get_det_id_desync};
use crate::{RecordReplayMode, ENV_LOGGER, RECORD_MODE,
            DESYNC_MODE, DesyncMode};
use crate::record_replay::recv_from_sender;
use std::thread::sleep;
use std::time::Duration;
use crate::record_replay::RecvErrorRR;
use std::cell::RefMut;
use log::Level::*;
use ipc_channel::ipc::{self};
use bincode;
pub use ipc_channel::ipc::{bytes_channel, IpcOneShotServer, IpcSharedMemory,
                           IpcBytesReceiver, IpcBytesSender};
use ipc_channel::platform::{OsIpcSharedMemory, OsOpaqueIpcChannel};
pub use ipc_channel::Error;
use crate::log_rr;
use serde::de::DeserializeOwned;
use serde::{ser::SerializeStruct, Deserialize, Serialize, Serializer};

use std::cell::RefCell;
use std::collections::hash_map::HashMap;
use std::collections::VecDeque;
use std::collections::HashSet;
use std::thread;
use ipc_channel::ipc::OpaqueIpcSender;

#[derive(Debug, Serialize, Deserialize)]
pub struct IpcReceiver<T> {
    pub(crate) receiver: ipc::IpcReceiver<(Option<DetThreadId>, T)>,
    buffer: RefCell<HashMap<Option<DetThreadId>, VecDeque<T>>>,
    pub(crate) metadata: RecordMetadata,
}

impl<T> IpcReceiver<T> {
    pub fn new(
        receiver: ipc::IpcReceiver<(Option<DetThreadId>, T)>,
        id: DetChannelId,
    ) -> IpcReceiver<T> {
        IpcReceiver {
            receiver,
            buffer: RefCell::new(HashMap::new()),
            metadata: RecordMetadata {
                type_name: unsafe { std::intrinsics::type_name::<T>().to_string() },
                flavor: FlavorMarker::Ipc,
                mode: *RECORD_MODE,
                id,
            },
        }
    }
}

impl<T> RecordReplayRecv<T, ipc_channel::Error> for IpcReceiver<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    fn to_recorded_event(
        &self,
        event: Result<(Option<DetThreadId>, T), ipc_channel::Error>,
    ) -> (Result<T, ipc_channel::Error>, Recorded) {
        match event {
            Ok((sender_thread, msg)) => (Ok(msg), Recorded::IpcRecvSucc { sender_thread }),
            Err(e) => (Err(e), Recorded::IpcRecvErr(IpcDummyError)),
        }
    }

    fn expected_recorded_events(&self, event: Recorded)
                                -> Result<Result<T, ipc_channel::Error>, DesyncError> {
        match event {
            Recorded::IpcRecvSucc { sender_thread } => {
                let retval = self.replay_recv(&sender_thread)?;
                // Here is where we explictly increment our event_id!
                inc_event_id();
                Ok(Ok(retval))
            }
            Recorded::IpcRecvErr(e) => {
                // TODO
                let err = "ErrorKing::Custom TODO".to_string();
                // Here is where we explictly increment our event_id!
                inc_event_id();
                Ok(Err(Box::new(ipc_channel::ErrorKind::Custom(err))))
            }
            e => {
                let mock_event = Recorded::IpcRecvSucc{ sender_thread: None };
                Err(DesyncError::EventMismatch(e, mock_event))
            }
        }
    }

    fn get_buffer(&self) -> RefMut<HashMap<Option<DetThreadId>, VecDeque<T>>> {
        self.buffer.borrow_mut()
    }
}

pub struct OpaqueIpcReceiver {
    opaque_receiver: ipc_channel::ipc::OpaqueIpcReceiver,
    pub(crate) metadata: RecordMetadata,
    /// For the IpceReceiverSet to work correctly, we keep track of when
    /// a OpaqueIpcReceiver is closed, to avoid adding it to the set
    /// of receivers when we desync. We track this information here.
    /// See: https://github.com/gatoWololo/rr_channel/issues/28
    closed: bool,
}

impl<T> IpcReceiver<T>
where T: for<'de> Deserialize<'de> + Serialize {
    fn rr_try_recv(&self) -> Result<(Option<DetThreadId>, T), RecvErrorRR> {
        for i in 0..10 {
            match self.receiver.try_recv() {
                Ok(v) => return Ok(v),
                Err(_) =>{
                    thread::sleep(Duration::from_millis(100))
                }
            }
        }
        Err(RecvErrorRR::Timeout)
    }

    pub fn recv(&self) -> Result<T, ipc_channel::Error> {
        let f = || self.receiver.recv();
        self.record_replay_recv(&self.metadata, f, "ipc_recv::recv()")
            .unwrap_or_else(|e| self.handle_desync(e, true, f))
    }

    pub fn try_recv(&self) -> Result<T, ipc_channel::Error> {
        let f = || self.receiver.try_recv();
        self.record_replay_recv(&self.metadata, f, "ipc_recv::try_recv()")
            .unwrap_or_else(|e| self.handle_desync(e, true, f))
    }

    pub fn to_opaque(self) -> OpaqueIpcReceiver {
        let metadata = self.metadata;
        OpaqueIpcReceiver {
            opaque_receiver: self.receiver.to_opaque(),
            metadata,
            closed: false,
        }
    }

    /// Deterministic receiver which loops until event comes from `sender`.
    /// All other entries are buffer is `self.buffer`. Buffer is always
    /// checked before entries.
    fn replay_recv(&self, sender: &Option<DetThreadId>) -> Result<T, DesyncError> {
        recv_from_sender(
            sender,
            || self.rr_try_recv(),
            &mut self.buffer.borrow_mut(),
            &self.metadata.id,
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpcSender<T> {
    sender: ipc::IpcSender<(Option<DetThreadId>, T)>,
    pub(crate) metadata: RecordMetadata,
}

impl<T> Clone for IpcSender<T>
where
    T: Serialize,
{
    fn clone(&self) -> Self {
        IpcSender {
            sender: self.sender.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

impl<T> RecordReplaySend<T, Error> for IpcSender<T>
where
    T: Serialize,
{
    fn check_log_entry(&self, entry: Recorded) -> Result<(), DesyncError> {
        match entry {
            Recorded::IpcSender => Ok(()),
            event_entry => Err(DesyncError::EventMismatch(event_entry, Recorded::IpcSender)),
        }
    }

    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), Error> {
        self.sender.send((thread_id, msg))
    }

    fn as_recorded_event(&self) -> Recorded {
        Recorded::IpcSender
    }
}

impl<T> IpcSender<T>
where
    T: Serialize,
{
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, data: T) -> Result<(), ipc_channel::Error> {
        match self.record_replay_send(
            data,
            &self.metadata.mode,
            &self.metadata.id,
            &self.metadata.type_name,
            &self.metadata.flavor,
            "IpcSender") {
            Ok(v) => v,
            Err((error, msg)) => {
                log_rr!(Warn, "IpcSend::Desynchronization detected: {:?}", error);
                match *DESYNC_MODE {
                    DesyncMode::Panic => panic!("IpcSend::Desynchronization detected: {:?}", error),
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

    pub fn to_opaque(self) -> OpaqueIpcSender {
        self.sender.to_opaque()
    }

    pub fn connect(name: String) -> Result<IpcSender<T>, std::io::Error> {
        let id = DetChannelId::new();

        let type_name = unsafe { std::intrinsics::type_name::<T>() };
        log_rr!(Info, "Sender connected created: {:?} {:?}", id, type_name);

        let metadata = RecordMetadata {
            type_name: type_name.to_string(),
            flavor: FlavorMarker::Ipc,
            mode: *RECORD_MODE,
            id,
        };
        ipc_channel::ipc::IpcSender::connect(name).
            map(|sender| IpcSender { sender, metadata })
    }
}

pub fn channel<T>() -> Result<(IpcSender<T>, IpcReceiver<T>), std::io::Error>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    *ENV_LOGGER;

    let (sender, receiver) = ipc::channel()?;
    let id = DetChannelId::new();
    let type_name = unsafe { std::intrinsics::type_name::<T>() };
    log_rr!(Info, "IPC channel created: {:?} {:?}", id, type_name);

    let metadata = RecordMetadata {
        type_name: type_name.to_string(),
        flavor: FlavorMarker::Ipc,
        mode: *RECORD_MODE,
        id: id.clone(),
    };

    Ok((
        IpcSender { sender, metadata },
        IpcReceiver::new(receiver, id),
    ))
}

pub enum IpcSelectionResult {
    /// A message received from the [IpcReceiver] in the [opaque] form,
    /// identified by the `u64` value.
    ///
    /// [IpcReceiver]: struct.IpcReceiver.html
    /// [opaque]: struct.OpaqueIpcMessage.html
    MessageReceived(u64, OpaqueIpcMessage),
    /// The channel has been closed for the [IpcReceiver] identified by the `u64` value.
    /// [IpcReceiver]: struct.IpcReceiver.html
    ChannelClosed(u64),
}

impl IpcSelectionResult {
    /// Helper method to move the value out of the [IpcSelectionResult] if it
    /// is [MessageReceived].
    ///
    /// # Panics
    ///
    /// If the result is [ChannelClosed] this call will panic.
    ///
    /// [IpcSelectionResult]: enum.IpcSelectionResult.html
    /// [MessageReceived]: enum.IpcSelectionResult.html#variant.MessageReceived
    /// [ChannelClosed]: enum.IpcSelectionResult.html#variant.ChannelClosed
    pub fn unwrap(self) -> (u64, OpaqueIpcMessage) {
        match self {
            IpcSelectionResult::MessageReceived(id, message) => (id, message),
            IpcSelectionResult::ChannelClosed(id) => {
                panic!("IpcSelectionResult::unwrap(): channel {} closed", id)
            }
        }
    }
}

#[derive(Debug)]
pub struct OpaqueIpcMessage {
    pub opaque: ipc_channel::ipc::OpaqueIpcMessage,
}


impl OpaqueIpcMessage {
    pub fn new(
        data: Vec<u8>,
        os_ipc_channels: Vec<OsOpaqueIpcChannel>,
        os_ipc_shared_memory_regions: Vec<OsIpcSharedMemory>,
    ) -> OpaqueIpcMessage {
        OpaqueIpcMessage {
            opaque: ipc_channel::ipc::OpaqueIpcMessage::new(
                data,
                os_ipc_channels,
                os_ipc_shared_memory_regions,
            ),
        }
    }
    pub fn to<T>(mut self) -> Result<T, bincode::Error>
    where
        T: for<'de> Deserialize<'de> + Serialize,
    {
        self.opaque.to::<(Option<DetThreadId>, T)>().map(|(_, v)| v)
    }
}

/// We assume that receivers will be added in deterministic order. We wrap the
/// router to ensure this is true for `RouterProxy`'s use of `IpcReceiverSet`.
/// If receivers are added in different order, this will cause replay to fail.
/// panic!()/warn!()
pub struct IpcReceiverSet {
    receiver_set: ipc_channel::ipc::IpcReceiverSet,
    mode: RecordReplayMode,
    /// On replay, we don't actually register receivers with the real IpcReceiverSet.
    /// Instead, we store them and assign them unique indices. This variable keeps
    /// tracks of the next idex to assing. It can be used to access the receivers
    /// through the `receivers` field.
    /// We use usize because that is what Vec uses.
    index: usize,
    /// On replay we don't use `receiver_set` instead we wait individually on
    /// the opaque receiver handles based on the recorded indices.
    /// TODO Should we "remove" receivers at some point? I feel like we should...
    /// Maybe this should be a vector...
    receivers: Vec<OpaqueIpcReceiver>,
    /// On multiple producer channels, sometimes we get the wrong event.
    /// buffer those here for later.
    /// Indexed first by channel index, then by expected Some(DetThreadId)
    /// (We hold messaged from None on this buffer as well).
    /// TODO: this buffer may grow unbounded, we never clean up old entries.
    buffer: HashMap<u64, HashMap<Option<DetThreadId>, VecDeque<OpaqueIpcMessage>>>,
    /// This `IpcReceiverSet` has become desynchronized. Any call to methods that
    /// return `DesyncError`s will return an error.
    desynced: bool,
    /// Holds on to senders counterparts for "dummy" receivers that have already
    /// been closed and then we desynchronized. We need to hang on to these
    /// receivers, but NEVER use them, have the "dummy" receivers act like
    /// closed receivers. We add sendes in abitrary order and nothing should be
    /// assumed about their ordering.
    /// See https://github.com/gatoWololo/rr_channel/issues/28
    dummy_senders: Vec<OpaqueIpcSender>
}

impl IpcReceiverSet {
    pub fn new() -> Result<IpcReceiverSet, std::io::Error> {
        ipc_channel::ipc::IpcReceiverSet::new().map(|r| IpcReceiverSet {
            receiver_set: r,
            mode: *RECORD_MODE,
            index: 0,
            receivers: Vec::new(),
            buffer: HashMap::new(),
            desynced: false,
            dummy_senders: Vec::new(),
        })
    }

    pub fn add<T>(&mut self, receiver: IpcReceiver<T>) -> Result<u64, std::io::Error>
    where
        T: for<'de> Deserialize<'de> + Serialize,
    {
        self.do_add(receiver.to_opaque())
    }

    pub fn add_opaque(&mut self, receiver: OpaqueIpcReceiver) -> Result<u64, std::io::Error> {
        self.do_add(receiver)
    }

    fn do_add(&mut self, receiver: OpaqueIpcReceiver) -> Result<u64, std::io::Error> {
        self.record_replay_add(receiver).unwrap_or_else(|(e, receiver)| {
            log_rr!(Warn, "IpcReceiver::add::Desynchonization detected: {:?}", e);
            match *DESYNC_MODE {
                DesyncMode::Panic => panic!("IpcReceiver::add::Desynchronization detected: {:?}", e),
                DesyncMode::KeepGoing => {
                    mark_program_as_desynced();
                    self.move_receivers_to_set();
                    inc_event_id();
                    self.receiver_set.add_opaque(receiver.opaque_receiver)
                }
            }
        })
    }

    pub fn select(&mut self) -> Result<Vec<IpcSelectionResult>, std::io::Error> {
        log_rr!(Info, "IpcSelect::select()");
        self.record_replay_select().unwrap_or_else(|(e, entries)| {
            log_rr!(Warn, "IpcSelect::Desynchonization detected: {:?}", e);

            match *DESYNC_MODE {
                DesyncMode::Panic => {
                    panic!("IpcSelect::Desynchronization detected: {:?}", e);
                }
                DesyncMode::KeepGoing => {
                    mark_program_as_desynced();
                    log_rr!(Trace, "Looking for entries in buffer or call to select()...");

                    self.move_receivers_to_set();
                    // First use up our buffered entries, if none. Do the
                    // actual wait.
                    match self.get_buffered_entries() {
                        None => {
                            log_rr!(Debug, "Doing direct do_select()");
                            inc_event_id();
                            self.do_select()
                        }
                        Some(be) => {
                            log_rr!(Debug, "Using buffered entries.");
                            inc_event_id();
                            Ok(be)
                        }
                    }
                }
            }
        })
    }

    /// By the time we hit a select. We assume all receivers have been added to this
    /// set. We verify this assumption by making all receivers be added in the order
    /// seen in the record. If not all receivers are present, it is a DesyncError.
    /// On DesyncError, we may be "halfway" through replaying a select, we return
    /// the entries we already have to avoid losing them.
    fn record_replay_select(&mut self) -> Result<Result<Vec<IpcSelectionResult>, std::io::Error>, (DesyncError, Vec<IpcSelectionResult>)> {
        if program_desyned() {
            return Err((DesyncError::Desynchronized, vec![]));
        }
        match self.mode {
            RecordReplayMode::Record => {
                // Events will be moved by our loop. So we put them back here.
                let mut moved_events: Vec<IpcSelectionResult> = Vec::new();
                let mut recorded_events = Vec::new();

                let selected = match self.do_select() {
                    Ok(v) => v,
                    e => return Ok(e),
                };

                // Iterate through events populating recorded_events.
                for e in selected {
                    match e {
                        IpcSelectionResult::MessageReceived(index, opaque_msg) => {
                            let det_id = IpcReceiverSet::deserialize_id(&opaque_msg);

                            moved_events
                                .push(IpcSelectionResult::MessageReceived(index, opaque_msg));
                            recorded_events.
                                push(IpcSelectEvent::MessageReceived(index, det_id));
                        }
                        IpcSelectionResult::ChannelClosed(index) => {
                            moved_events.push(IpcSelectionResult::ChannelClosed(index));
                            recorded_events.push(IpcSelectEvent::ChannelClosed(index));
                        }
                    }
                }

                let event = Recorded::IpcSelect { select_events: recorded_events };
                // Ehh, we fake it here. We never check this value anyways.
                let id = &DetChannelId::fake();
                record_replay::log(event, FlavorMarker::IpcSelect, "IpcSelect", id);
                Ok(Ok(moved_events))
            }

            // Use index to fetch correct receiver and read value directly from
            // receiver.
            RecordReplayMode::Replay => {
                if self.desynced {
                    return Err((DesyncError::Desynchronized, vec![]));
                }
                let det_id = get_det_id_desync().map_err(|e| (e, vec![]))?;

                // Here we put this thread to sleep if the entry is missing.
                // On record, the thread never returned from blocking...
                let entry = get_log_entry(det_id, get_event_id());
                if let Err(e@DesyncError::NoEntryInLog(_, _)) = entry {
                    log_rr!(Info, "Saw {:?}. Putting thread to sleep.", e);
                    loop { thread::park() }
                }

                match entry.map_err(|e| (e, vec![]))? {
                    Recorded::IpcSelect { select_events } => {
                        let mut events: Vec<IpcSelectionResult> = Vec::new();

                        for event in select_events {
                            match self.replay_select_event(event) {
                                Err((e, entry)) => {
                                    events.push(entry);
                                    return Err((e, events));
                                }
                                Ok(entry) => events.push(entry),
                            }
                        }

                        inc_event_id();
                        Ok(Ok(events))
                    }
                    event => {
                        let dummy = Recorded::IpcSelect { select_events: vec![] };
                        Err((DesyncError::EventMismatch(event.clone(), dummy), vec![]))
                    }
                }
            }
            RecordReplayMode::NoRR => Ok(self.do_select()),
        }
    }

    /// From a IpcSelectEvent, fetch the correct IpcSelectionResult.
    /// On error return the event that caused Desync.
    fn replay_select_event(&mut self, event: &IpcSelectEvent)
                           -> Result<IpcSelectionResult, (DesyncError, IpcSelectionResult)> {
        log_rr!(Trace, "replay_select_event for {:?}", event);
        match event {
            IpcSelectEvent::MessageReceived(index, expected_sender) => {
                if expected_sender.is_none() {
                    // Since we don't have a expected_sender, it may be the case this
                    // is nondeterminism if there are multiple producers both writing
                    // to this channel as None.
                    log_rr!(Warn, "Sender ID is None. \
                                   This execution may be nondeterministic");
                }
                let receiver = self.receivers.get_mut(*index as usize).
                // This should never happen. The way the code is written, by the
                // time we get here, all receivers have been registered with us correctly.
                // If any of them are missing. We would have seen it already.
                // expect here represents a bug on the internal assumptions of our code.
                    expect("Unable to fetch correct receiver from map.");

                // Auto buffers results.
                Ok(IpcReceiverSet::do_replay_recv_for_entry(&mut self.buffer,
                                                         *index,
                                                         expected_sender,
                                                         receiver)?)
            }
            IpcSelectEvent::ChannelClosed(index) => {
                let receiver = self.receivers.get_mut(*index as usize).
                // This should never happen. The way the code is written, by the
                // time we get here, all receivers have been registered with us correctly.
                // If any of them are missing. We would have seen it already.
                // expect here represents a bug on the internal assumptions of our code.
                    expect("Unable to fetch correct receiver from map.");

                match receiver.opaque_receiver.os_receiver.recv() {
                    Err(e) if e.channel_is_closed() => {
                        log_rr!(Trace,
                                "replay_select_event(): Saw channel closed for {:?}", index);
                        // remember that this receiver closed!
                        receiver.closed = true;
                        Ok(IpcSelectionResult::ChannelClosed(*index))
                    }
                    Err(e) => {
                        panic!("Unknown reason for error: {:?}", e);
                    }
                    Ok(val) => {
                        // Expected a channel closed, saw a real message...
                        log_rr!(Trace, "Expected channel closed! Saw success: {:?}", index);
                        Err((DesyncError::ChannelClosedExpected,
                             IpcReceiverSet::convert(*index, val.0, val.1, val.2)))
                    }
                }
            }
        }
    }

    /// TODO: There is borrowing issues if I mut borrow the receiver, and
    /// have this function be a method with (&mut self). So I pass everything
    /// explicitly for now.

    /// Loop until correct message is found for passed receiver. If the message
    /// is from the "wrong" sender, it is buffered.
    /// DesyncError is returned if the channel closes on us. We return the
    /// Channel closed event.
    fn do_replay_recv_for_entry(
        buffer: &mut HashMap<u64, HashMap<Option<DetThreadId>, VecDeque<OpaqueIpcMessage>>>,
        index: u64,
        expected_sender: &Option<DetThreadId>,
        receiver: &mut OpaqueIpcReceiver,
    ) -> Result<IpcSelectionResult, (DesyncError, IpcSelectionResult)> {
        log_rr!(Debug, "do_replay_recv_for_entry");

        if let Some(entry) = buffer.
            get_mut(&index).
            and_then(|m| m.get_mut(expected_sender)).
            and_then(|e| e.pop_front()) {
                log_rr!(Trace, "IpcSelect(): Recv message found in buffer.");
                return Ok(IpcSelectionResult::MessageReceived(index, entry));
            }

        loop {
            let (data, channels, shared_memory_regions) =
                match receiver.opaque_receiver.os_receiver.recv() {
                    Ok(v) => v,
                    Err(e) if e.channel_is_closed() => {
                        log_rr!(Trace, "Expected message, saw channel closed.");
                        // remember that this receiver closed!
                        receiver.closed = true;
                        return Err((DesyncError::ChannelClosedUnexpected(index),
                                   IpcSelectionResult::ChannelClosed(index)));
                    }
                    Err(e) => panic!("do_replay_recv_for_entry: \
                                      Unknown reason for error: {:?}", e),
            };

            let msg = OpaqueIpcMessage::new(data, channels, shared_memory_regions);
            let det_id = IpcReceiverSet::deserialize_id(&msg);

            if &det_id == expected_sender {
                log_rr!(Trace, "do_replay_recv_for_entry: message found via recv()");
                return Ok(IpcSelectionResult::MessageReceived(index, msg));
            } else {
                log_rr!(Trace, "Wrong message received from {:?}, adding it to buffer",
                          det_id);
                buffer
                    .entry(index)
                    .or_insert(HashMap::new())
                    .entry(det_id)
                    .or_insert(VecDeque::new())
                    .push_back(msg);
            }
        }
    }

    /// Call actual select for real IpcReceiverSet and convert their
    /// ipc_channel::ipc::IpcSelectionResult into our IpcSelectionResult.
    fn do_select(&mut self) -> Result<Vec<IpcSelectionResult>, std::io::Error> {
        log_rr!(Trace, "IpcReceiverSet::do_select()");

        let selected = self.receiver_set.select()?;
        let selected: Vec<IpcSelectionResult> = selected.into_iter()
            .map(|selection| match selection {
                ipc_channel::ipc::IpcSelectionResult::MessageReceived(i, opaque) => {
                    IpcSelectionResult::MessageReceived(i, OpaqueIpcMessage { opaque })
                }
                ipc_channel::ipc::IpcSelectionResult::ChannelClosed(i) => {
                    IpcSelectionResult::ChannelClosed(i)
                }
            })
            .collect();
        log_rr!(Info, "Found {:?} entries.", selected.len());
        Ok(selected)
    }

    /// On error we return the back to the user, otherwise it would be moved forever.
    fn record_replay_add(&mut self, receiver: OpaqueIpcReceiver)
                         -> Result<Result<u64, std::io::Error>, (DesyncError, OpaqueIpcReceiver)> {
        if program_desyned() {
            return Err((DesyncError::Desynchronized, receiver));
        }
        let metadata = receiver.metadata.clone();
        let flavor = metadata.flavor;
        let id = metadata.id;
        log_rr!(Debug, "IpcSelect::record_replay_add<{:?}>()", id);

        // After the first time we desynchronize, this is set to true and future times
        // we eill be sent here.
        if self.desynced {
            return Err((DesyncError::Desynchronized, receiver));
        }

        match self.mode {
            RecordReplayMode::Record => {
                let index = match self.receiver_set.add_opaque(receiver.opaque_receiver) {
                    Ok(v) => v,
                    e@Err(_) => return Ok(e),
                };

                let event = Recorded::IpcSelectAdd(index);
                record_replay::log(event, flavor, &metadata.type_name, &id);
                Ok(Ok(index))
            }
            // Do not add receiver to IpcReceiverSet, instead move the receiver
            // to our own `receivers` hashmap where the index returned here is
            // the key.
            RecordReplayMode::Replay => {
                let det_id = match get_det_id_desync() {
                    Err(e) => return Err((e, receiver)),
                    Ok(v) => v,
                };
                let log_entry = match get_log_entry_with(det_id, get_event_id(), &flavor, &id) {
                    Err(e) => return Err((e, receiver)),
                    Ok(v) => v,
                };
                let index = self.index as u64;
                match log_entry {
                    Recorded::IpcSelectAdd(r_index) => {
                        if *r_index != index {
                            let error = DesyncError::SelectIndexMismatch(*r_index, index);
                            return Err((error, receiver));
                        }
                        if let Some(_) = self.receivers.get(self.index) {
                            let error = DesyncError::SelectExistingEntry(index);
                            return Err((error, receiver));
                        }

                        self.receivers.insert(self.index, receiver);

                        self.index += 1;
                        inc_event_id();
                        Ok(Ok(index))
                    }
                    event => {
                        let dummy = Recorded::IpcSelectAdd((0 - 1) as u64);
                        Err((DesyncError::EventMismatch(event.clone(), dummy), receiver))
                    }
                }
            }
            RecordReplayMode::NoRR => {
                Ok(self.receiver_set.add_opaque(receiver.opaque_receiver))
            }
        }
    }

    /// Fetch all buffered entries and treat them as the results of a
    /// select.
    fn get_buffered_entries(&mut self) -> Option<Vec<IpcSelectionResult>> {
        log_rr!(Trace, "Desynchronized detected. Fetching buffered Entries.");
        let mut entries = vec![];
        // Add all events that we have buffered up to this point if any.
        for (index, mut hashmap) in self.buffer.drain() {
            for (_, mut queue) in hashmap.drain() {
                for entry in queue.drain(..) {
                    log_rr!(Trace, "Entry found through buffer!");
                    entries.push(IpcSelectionResult::MessageReceived(index, entry));
                }
            }
        }
        if entries.len() == 0 {
            log_rr!(Trace, "No buffered entries.");
            None
        } else {
            for e in &entries {
                match e {
                    IpcSelectionResult::MessageReceived(i, _) => {
                        log_rr!(Trace, "IpcSelectionResult::MessageReceived({:?}, _)", i);
                    }
                    IpcSelectionResult::ChannelClosed(i) => {
                        log_rr!(Trace, "IpcSelectionResult::ChannelClosed({:?}, _)", i);
                    }
                }
            }
            Some(entries)
        }
    }

    /// We have detected a desynchonization. Get all receivers in our map and add them
    /// to the actual set as we will be calling methods on the set directly instead
    /// of faking it.
    fn move_receivers_to_set(&mut self) {
        // Move all our receivers into the receiver set.
        if ! self.desynced {
            log_rr!(Debug, "Moving all receivers into actual select set.");

            // IMPORTANT: Vec ensures that element are drained in order they were added!
            // This is necessary for the receiver set indices to match.
            for r in self.receivers.drain(..) {
                if r.closed {
                    // This receiver has closed. Use dummy receiver to avoid duplicating
                    // closed message.

                    // We don't know the correct type of this message, but that is
                    // okay. We will never read from this receiver.
                    let (s, r) = channel::<()>().
                        expect("Unable to create channels for dummy receiver");
                    // keep sender around forever to avoid dropping channel.
                    self.dummy_senders.push(s.to_opaque());
                    let index = self.receiver_set.add_opaque(r.to_opaque().opaque_receiver).
                        expect("Unable to add dummy receiver while \
                                handling desynchronization.");
                    log_rr!(Trace, "Adding dummy receiver at index {:?}", index);
                } else {
                    // Add the real receiver.
                    let id = r.metadata.id.clone();
                    let i = self.receiver_set.add_opaque(r.opaque_receiver).
                        expect("Unable to add receiver while handling desynchronization.");

                    log_rr!(Trace, "move_receivers_to_set: Added receiver {:?} with index: {:?}",
                           id, i);
                }


            }
            self.desynced = true;
        }
    }

    /// Extract DetThreadId from opaque message. Not easy :P
    fn deserialize_id(opaque_msg: &OpaqueIpcMessage) -> Option<DetThreadId> {
        let (det_id, _): (Option<DetThreadId>, /*Unknown T*/ ()) =
            bincode::deserialize(&opaque_msg.opaque.data)
            .expect("Unable to deserialize DetThreadId");
        det_id
    }

    fn convert(index: u64,
               data: Vec<u8>,
               os_ipc_channels: Vec<OsOpaqueIpcChannel>,
               os_ipc_shared_memory_regions: Vec<OsIpcSharedMemory>) ->
        IpcSelectionResult {
        let msg = OpaqueIpcMessage::new(data,
                                        os_ipc_channels,
                                        os_ipc_shared_memory_regions);
        IpcSelectionResult::MessageReceived(index, msg)
    }
}
