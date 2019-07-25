use crate::log_trace;
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
pub use ipc_channel::ipc::OpaqueIpcReceiver;
pub use ipc_channel::ipc::bytes_channel;
pub use ipc_channel::ipc::IpcSharedMemory;
pub use ipc_channel::ipc::{IpcSelectionResult, OpaqueIpcMessage};
pub use ipc_channel::ipc::{IpcBytesReceiver, IpcBytesSender};
pub use ipc_channel::Error;
pub use ipc_channel::ipc::IpcOneShotServer;
use crate::record_replay::DetChannelId;
use crate::record_replay::RecordReplaySend;

// pub struct OpaqueIpcReceiver {
    // ???
// }

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
        self.receiver.to_opaque()
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

/// OMAR: Wrapper necessary to change use our own IpcReceiver type for add.
/// TODO Will probably need to determize select operation.
pub struct IpcReceiverSet {
    receiver_set: ipc_channel::ipc::IpcReceiverSet,
    mode: RecordReplayMode,
    det_ids: HashMap<u64, DetChannelId>
}

impl IpcReceiverSet {
    pub fn new() -> Result<IpcReceiverSet, std::io::Error> {
        ipc_channel::ipc::IpcReceiverSet::new().
            map(|r| IpcReceiverSet { receiver_set: r,
                                     mode: *RECORD_MODE,
                                     det_ids: HashMap::new(),
            })
    }

    pub fn add<T>(&mut self, receiver: IpcReceiver<T>) -> Result<u64, std::io::Error>
    where T: for<'de> Deserialize<'de> + Serialize {
        // Receivers may be added in nondeteminisc order. So we cannot rely on the
        // index returned from `add` to replay like we do with crossbeam channels.
        // Instead we keep DetChannelId around and map it to its index.
        // TODO
        // let chan_id = receiver.metadata.id;
        self.receiver_set.add(receiver.receiver)
            // On success register receiver
            // map(|index| {
                // let err = |v| -> Option<u32> {
                //     panic!(format!("Entry already exists in det_ids: {:?}", v));
                // };
                // self.det_ids.insert(index, chan_id).and_then(err);
                // index
        // })
    }

    pub fn add_opaque(&mut self, receiver: OpaqueIpcReceiver) -> Result<u64, std::io::Error> {
        self.receiver_set.add_opaque(receiver)
    }

    pub fn select(&mut self) -> Result<Vec<IpcSelectionResult>, std::io::Error> {
        self.receiver_set.select()
    }
    //     match self.mode {
    //         RecordReplayMode::Record => {
    //             // Record DetChannelIds of all events so we can replay them later.
    //             let events = self.receiver_set.select()?;

    //             for e in &events {
    //                 match e {
    //                     IpcSelectionResult::MessageReceived(index, msg) => {
    //                         // log message received.
    //                         // How to get original message back?
    //                         // msg.to()
    //                         unimplemented!()
    //                     }
    //                     IpcSelectionResult::ChannelClosed(index) => {
    //                         // log channel closed envent.
    //                     }
    //                 }
    //             }
    //             Ok(events)
    //         }
    //         RecordReplayMode::Replay => {
    //             unimplemented!()

    //         }
    //         RecordReplayMode::NoRR => {
    //             unimplemented!()
    //         }
    //     }
    // }
}
