use crate::log_trace;
use log::warn;
use crate::record_replay::RecordMetadata;
use crate::record_replay::{self, FlavorMarker, IpcDummyError, RecordReplay, Recorded, Blocking};
use crate::thread::get_and_inc_channel_id;
use crate::thread::get_det_id;
use crate::thread::DetThreadId;
use crate::RecordReplayMode;
use crate::ENV_LOGGER;
use crate::RECORD_MODE;
use ipc_channel::ipc::{self};
// use ipc_channel::{Error, ErrorKind};
use crate::thread::inc_event_id;
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use std::cell::RefCell;
use std::collections::hash_map::HashMap;
use std::collections::VecDeque;
use serde::Serializer;
use serde::de::DeserializeOwned;
pub use ipc_channel::ipc::bytes_channel;
pub use ipc_channel::ipc::IpcSharedMemory;
pub use ipc_channel::ipc::{IpcBytesReceiver, IpcBytesSender};
pub use ipc_channel::Error;
pub use ipc_channel::ipc::IpcOneShotServer;
use crate::record_replay::DetChannelId;
use ipc_channel::platform::OsOpaqueIpcChannel;
use crate::record_replay::RecordReplaySend;
use ipc_channel::platform::OsIpcSharedMemory;
use crate::record_replay::get_log_entry;
use crate::record_replay::IpcSelectEvent;
use crate::thread::get_event_id;
use crate::log_trace_with;
use bincode;

#[derive(Debug, Serialize, Deserialize)]
pub struct IpcReceiver<T>
{
    pub(crate) receiver: ipc::IpcReceiver<(Option<DetThreadId>, T)>,
    buffer: RefCell<HashMap<DetThreadId, VecDeque<T>>>,
    none_buffer: RefCell<VecDeque<T>>,
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
            none_buffer: RefCell::new(VecDeque::new()),
            metadata: RecordMetadata {
                type_name: unsafe { std::intrinsics::type_name::<T>().to_string() },
                flavor: FlavorMarker::Ipc,
                mode: *RECORD_MODE,
                id,
            },
        }
    }
}

impl<T> RecordReplay<T, ipc_channel::Error> for IpcReceiver<T>
     where T: for<'de> Deserialize<'de> + Serialize
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

    fn expected_recorded_events(&self, event: &Recorded) -> Result<T, ipc_channel::Error> {
        match event {
            Recorded::IpcRecvSucc { sender_thread } => {
                let retval = self.replay_recv(sender_thread);
                // Here is where we explictly increment our event_id!
                inc_event_id();
                Ok(retval)
            }
            Recorded::IpcRecvErr(e) => {
                let err = "ErrorKing::Custom TODO".to_string();
                // Here is where we explictly increment our event_id!
                inc_event_id();
                Err(Box::new(ipc_channel::ErrorKind::Custom(err)))
            }
            e => {
                let error = format!("Unexpected event: {:?} in replay for ipc_channel", e);
                log_trace(&error);
                panic!(error);
            }
        }
    }
}

pub struct OpaqueIpcReceiver {
    opaque_receiver: ipc_channel::ipc::OpaqueIpcReceiver,
    metadata: RecordMetadata,
}

impl<T> IpcReceiver<T>
    where T: for<'de> Deserialize<'de> + Serialize
{
    pub fn recv(&self) -> Result<T, ipc_channel::Error> {
        self.record_replay(&self.metadata, || self.receiver.recv(),
                           Blocking::CanBlock,"ipc_recv::recv()")
    }

    pub fn try_recv(&self) -> Result<T, ipc_channel::Error> {
        self.record_replay(&self.metadata, || self.receiver.try_recv(),
                           Blocking::CannotBlock, "ipc_recv::try_recv()")
    }

    pub fn to_opaque(self) -> OpaqueIpcReceiver {
        let metadata = self.metadata;
        OpaqueIpcReceiver {
            opaque_receiver: self.receiver.to_opaque(),
            metadata,
        }

    }

    fn replay_recv(&self, sender: &Option<DetThreadId>) -> T {
        record_replay::replay_recv(
            sender,
            || self.receiver.recv(),
            &mut self.buffer.borrow_mut(),
            &mut self.none_buffer.borrow_mut(),
            &self.metadata.id,
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpcSender<T> {
    sender: ipc::IpcSender<(Option<DetThreadId>, T)>,
    pub(crate) metadata: RecordMetadata,
}

impl<T> Clone for IpcSender<T> where T: Serialize,
{
    fn clone(&self) -> Self {
        IpcSender {
            sender: self.sender.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

impl<T> RecordReplaySend<T, Error> for IpcSender<T> where T: Serialize {
    fn check_log_entry(&self, entry: Recorded) {
        match entry {
            Recorded::IpcSender(chan_id) => {
                if chan_id != self.metadata.id {
                    panic!("Expected {:?}, saw {:?}", chan_id, self.metadata.id)
                }
            }
            _ => panic!("Expected Recorded::Sender. Saw: {:?}", entry),
        }
    }

    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), Error> {
        self.sender.send((thread_id, msg))
    }

    fn to_recorded_event(&self, id: DetChannelId) -> Recorded {
        Recorded::IpcSender(id)
    }
}

impl<T> IpcSender<T> where T: Serialize,
{
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, data: T) -> Result<(), ipc_channel::Error> {
        self.record_replay_send(
            data,
            &self.metadata.mode,
            &self.metadata.id,
            &self.metadata.type_name,
            &self.metadata.flavor,
            "IpcSender",
        )
    }

    pub fn connect(name: String) -> Result<IpcSender<T>, std::io::Error> {
        let id = DetChannelId {
            det_thread_id: get_det_id(),
            channel_id: get_and_inc_channel_id(),
        };

        let type_name = unsafe { std::intrinsics::type_name::<T>() };
        log_trace(&format!("Sender connected created: {:?} {:?}", id, type_name));

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
    let id = DetChannelId {
        det_thread_id: get_det_id(),
        channel_id: get_and_inc_channel_id(),
    };
    let type_name = unsafe { std::intrinsics::type_name::<T>() };
    log_trace(&format!("IPC channel created: {:?} {:?}", id, type_name));

    let metadata = RecordMetadata {
        type_name: type_name.to_string(),
        flavor: FlavorMarker::Ipc,
        mode: *RECORD_MODE,
        id: id.clone(),
    };

    Ok((
        IpcSender {
            sender,
            metadata,
        },
        IpcReceiver::new(receiver, id),
    ))
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
    index: u64,
    /// On replay we don't use `receiver_set` instead we wait individually on
    /// the opaque receiver handles based on the recorded indices.
    receivers: HashMap<u64, OpaqueIpcReceiver>,
    /// On multiple producer channels, sometimes we get the wrong event.
    /// buffer those here for later.
    /// Indexed first by channel index, then by expected Some(DetThreadId)
    /// (We hold messaged from None on this buffer as well).
    buffer: HashMap<u64, HashMap<Option<DetThreadId>, VecDeque<OpaqueIpcMessage>>>,
}

#[derive(Debug)]
pub struct OpaqueIpcMessage {
    pub opaque: ipc_channel::ipc::OpaqueIpcMessage,
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

impl OpaqueIpcMessage {
       pub fn new(data: Vec<u8>,
           os_ipc_channels: Vec<OsOpaqueIpcChannel>,
           os_ipc_shared_memory_regions: Vec<OsIpcSharedMemory>)
                  -> OpaqueIpcMessage {
           OpaqueIpcMessage {
               opaque : ipc_channel::ipc::OpaqueIpcMessage::new(
                   data, os_ipc_channels, os_ipc_shared_memory_regions)
           }
       }
    pub fn to<T>(mut self) -> Result<T, bincode::Error>
    where T: for<'de> Deserialize<'de> + Serialize {
        self.opaque.to::<(Option<DetThreadId>, T)>().map(|(_, v)| v)
    }
}

impl IpcReceiverSet {
    pub fn new() -> Result<IpcReceiverSet, std::io::Error> {
        ipc_channel::ipc::IpcReceiverSet::new().
            map(|r|
                IpcReceiverSet { receiver_set: r,
                                 mode: *RECORD_MODE,
                                 index: 0,
                                 receivers: HashMap::new(),
                                 buffer: HashMap::new(),
            })
    }

    pub fn add<T>(&mut self, receiver: IpcReceiver<T>) -> Result<u64, std::io::Error>
    where T: for<'de> Deserialize<'de> + Serialize {
        self.rr_add(receiver.to_opaque())
    }

    pub fn add_opaque(&mut self, receiver: OpaqueIpcReceiver) -> Result<u64, std::io::Error> {
        self.rr_add(receiver)
    }

    /// By the time we hit a select. We assume all receivers have been added to this
    /// set. We verify this assumption by making all receivers be added in the order
    /// seen in the record. If not all receivers are present, we panic.
    /// (TODO switch to non panicking)
    pub fn select(&mut self) -> Result<Vec<IpcSelectionResult>, std::io::Error> {
        log_trace("IpcSelect::select()");

        match self.mode {
            // Record which events returned.
            RecordReplayMode::Record => {
                // Events will be moved by our loop. So we put them back here.
                let mut moved_events: Vec<IpcSelectionResult> = Vec::new();
                let mut recorded_events = Vec::new();

                // Iterate through events populating recorded_events.
                for e in self.do_select()? {
                    match e {
                        // Extrace DetThreadId from opaque message. Not easy :P
                        IpcSelectionResult::MessageReceived(index, opaque_message) => {
                            let (det_id, _): (Option<DetThreadId>, /*Unknown T*/()) =
                                bincode::deserialize(&opaque_message.opaque.data).
                                expect("Unable to deserialize DetThreadId");

                            recorded_events.push(IpcSelectEvent::MessageReceived(index, det_id));
                            moved_events.push(
                                IpcSelectionResult::MessageReceived(index, opaque_message));
                        }
                        IpcSelectionResult::ChannelClosed(index) => {
                            moved_events.push(IpcSelectionResult::ChannelClosed(index));
                            recorded_events.push(IpcSelectEvent::ChannelClosed(index));
                        }
                    }
                }
                let event = Recorded::IpcSelect{select_events: recorded_events};
                // Ehh, we fake it here. We never check this value anyways.
                let id = &DetChannelId { det_thread_id: None, channel_id: 0 };
                record_replay::log(event, FlavorMarker::IpcSelect, "IpcSelect", id);
                Ok(moved_events)
            }
            // Use index to fetch correct receiver and read value directly from
            // receiver.
            RecordReplayMode::Replay => {
                let mut events: Vec<IpcSelectionResult> = Vec::new();
                let det_id = get_det_id().expect("Select thread's det id not set.");

                match get_log_entry(det_id, get_event_id()) {
                    None => {
                        log_trace("No entry for IpcReceiverSet::select()");
                        // No entry present. Assume this select never returned!
                        log_trace("Putting thread to sleep!");
                        loop {
                            std::thread::park();
                            log_trace("Spurious wakeup, going back to sleep.");
                        }
                    }
                    Some((Recorded::IpcSelect {select_events}, _, _)) => {
                        for event in select_events {
                            match event {
                                IpcSelectEvent::MessageReceived(index, expected_sender) => {
                                    if expected_sender.is_none() {
                                        // Since we don't have a expected_sender, it may be the case this
                                        // is nondeterminism if there are multiple producers both writing
                                        // to this channel as None.
                                        warn!("Expected sender is None. This execution may be nondeterministic");
                                    }
                                    // Fetch the receiver and explicitly wait the message.
                                    let receiver = self.receivers.get_mut(index).
                                        expect("Missing key in receivers");
                                    // Auto buffers results.
                                    let entry = IpcReceiverSet::do_replay_recv_for_entry(
                                        &mut self.buffer,
                                        *index,
                                        expected_sender,
                                        receiver);
                                    events.push(entry);
                                }
                                IpcSelectEvent::ChannelClosed(index) => {
                                    let receiver = self.receivers.get_mut(index).
                                        expect("Missing key in receivers");
                                    match receiver.opaque_receiver.os_receiver.recv() {
                                        // TODO Check if this is some random error
                                        // or a channel closed error!!!
                                        Err(e) => {
                                            events.push(IpcSelectionResult::ChannelClosed(*index))
                                        }
                                        Ok(_) => panic!("Expected ChannelClosed!"),
                                    }
                                }
                            }
                        }
                        inc_event_id();
                        Ok(events)
                    }
                    Some((e, _, _)) => {
                        log_trace(&format!("IpcReceiverSet::select(): Unexpected event: {:?}", e));
                        panic!("IpcReceiverSet::select(): Unexpected event: {:?}", e);
                    }


                }
            }
            RecordReplayMode::NoRR => {
                self.do_select()
            }
        }
    }

    /// TODO: There is borrowing issues if I mut borrow the receiver, and
    /// have this function be a method with (&mut self). So I pass everything
    /// explicitly for now.
    fn do_replay_recv_for_entry(
        buffer: &mut HashMap<u64, HashMap<Option<DetThreadId>, VecDeque<OpaqueIpcMessage>>>,
        index: u64,
        expected_sender: &Option<DetThreadId>,
        receiver: &mut OpaqueIpcReceiver)
        -> IpcSelectionResult {

        // Check our buffer to see if this value is already here.
        if let Some(inner_map) = buffer.get_mut(&index){
            if let Some(entries) = inner_map.get_mut(expected_sender) {
                if let Some(entry) = entries.pop_front() {
                    log_trace("IpcSelect(): Recv message found in buffer.");
                    return IpcSelectionResult::MessageReceived(index, entry);
                }
            }
        }

        loop {
            let (data,
                 os_ipc_channels,
                 os_ipc_shared_memory_regions) =
                receiver.opaque_receiver.os_receiver.recv().
                expect("failed to recv() from OS receiver.");

            // TODO: Yuck. Ugly. Refactor.
            let msg = OpaqueIpcMessage::new(data,
                                            os_ipc_channels,
                                            os_ipc_shared_memory_regions);
            let (det_id, _):
            (Option<DetThreadId>, /*Unknown T*/()) =
                bincode::deserialize(&msg.opaque.data).
                expect("Unable to deserialize DetThreadId");

            if &det_id == expected_sender {
                return IpcSelectionResult::MessageReceived(index, msg);
            } else {
                log_trace(&format!("Wrong message received from {:?}, adding it to buffer",
                                   det_id));
                buffer.entry(index).
                    or_insert(HashMap::new()).
                    entry(det_id).
                    or_insert(VecDeque::new()).
                    push_back(msg);
            }
        }
    }

    /// Call actual select for real IpcReceiverSet and convert their
    /// ipc_channel::ipc::IpcSelectionResult into our IpcSelectionResult.
    fn do_select(&mut self) -> Result<Vec<IpcSelectionResult>, std::io::Error> {
        // TODO: Ugly. refactor.
        let v =self.receiver_set.select()
            .expect("Cannot do_select...").
            into_iter().
            map(|selection| {
                match selection {
                    ipc_channel::ipc::IpcSelectionResult::MessageReceived(i, opaque) => {
                        let m = OpaqueIpcMessage { opaque };
                        IpcSelectionResult::MessageReceived(i, m)
                    }
                    ipc_channel::ipc::IpcSelectionResult::ChannelClosed(i) => {
                        IpcSelectionResult::ChannelClosed(i)
                    }
                }
            }).collect();
        Ok(v)
    }

    fn rr_add(&mut self, receiver: OpaqueIpcReceiver)
              -> Result<u64, std::io::Error> {
        let metadata = receiver.metadata.clone();
        let flavor = metadata.flavor;
        let id = metadata.id;
        log_trace(&format!("IpcSelect::rr_add<{:?}>()", id));

        match self.mode {
            RecordReplayMode::Record => {
                let type_name = metadata.type_name;

                let index = self.receiver_set.add_opaque(receiver.opaque_receiver).
                    expect("Did not expect add() to fail...");

                let event = Recorded::IpcSelectAdd(index);
                record_replay::log(event, flavor, &type_name, &id);
                Ok(index)
            }
            // Do not add receiver to IpcReceiverSet, intead move the receiver
            // to our own `receivers` hashmap where the index returned here is
            // the key.
            RecordReplayMode::Replay => {
                let det_id = get_det_id().
                    expect("None found on thread calling IpcReceiver add");
                match get_log_entry(det_id, get_event_id()) {
                    Some((Recorded::IpcSelectAdd(r_index), r_flavor, r_id)) => {
                        // TODO change to assert EQ?
                        if *r_flavor != flavor {
                            panic!("Expected {:?}, saw {:?}", r_flavor, flavor);
                        }
                        if *r_id != id {
                            panic!("Expected {:?}, saw {:?}", r_id, id);
                        }

                        // Add our entry to map here.
                        if let Some(e) = self.receivers.insert(self.index, receiver) {
                            panic!("Map entry already exists.");
                        }

                        if *r_index != self.index {
                            panic!("IpcReceiverSet::rr_add. Wrong index. Expected {:?}, saw {:?}", r_index, self.index);
                        }

                        let index = self.index;
                        self.index += 1;
                        inc_event_id();
                        Ok(index)
                    }
                    Some((r, _, _)) => {
                        panic!("Expected IpcReceiverSetAddSucc instead saw {:?}", r);
                    }
                    None => {
                        panic!("Missing entry for IpcReceiverSet::add()");
                    }
                }
            }
            RecordReplayMode::NoRR => {
                self.receiver_set.add_opaque(receiver.opaque_receiver)
            }
        }
    }

}
