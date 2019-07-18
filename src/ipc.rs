use crate::log_trace;
use crate::record_replay::RecordMetadata;
use crate::record_replay::{self, FlavorMarker, IpcDummyError, RecordReplay, Recorded};
use crate::thread::get_and_inc_channel_id;
use crate::thread::get_det_id;
use crate::thread::DetThreadId;
use crate::RecordReplayMode;
use crate::ENV_LOGGER;
use crate::RECORD_MODE;
use ipc_channel::ipc::{self};
use ipc_channel::{Error, ErrorKind};

use serde::{Deserialize, Serialize, ser::SerializeStruct};
use std::cell::RefCell;
use std::collections::hash_map::HashMap;
use std::collections::VecDeque;
use serde::Serializer;
use serde::de::DeserializeOwned;

pub use ipc_channel::ipc::{
    IpcSelectionResult, OpaqueIpcMessage, OpaqueIpcReceiver
};
#[derive(Debug, Serialize, Deserialize)]
pub struct IpcReceiver<T>
{
    pub(crate) receiver: ipc::IpcReceiver<(DetThreadId, T)>,
    buffer: RefCell<HashMap<DetThreadId, VecDeque<T>>>,
    metadata: RecordMetadata,
}

impl<T> IpcReceiver<T> {
    pub fn new(
        receiver: ipc::IpcReceiver<(DetThreadId, T)>,
        id: (DetThreadId, u32),
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

impl<T> RecordReplay<T, Error> for IpcReceiver<T>
     where T: for<'de> Deserialize<'de> + Serialize
{
    fn to_recorded_event(
        &self,
        event: Result<(DetThreadId, T), Error>,
    ) -> (Result<T, Error>, Recorded) {
        match event {
            Ok((sender_thread, msg)) => (Ok(msg), Recorded::IpcRecvSucc { sender_thread }),
            Err(e) => (Err(e), Recorded::IpcRecvErr(IpcDummyError)),
        }
    }

    fn expected_recorded_events(&self, event: Recorded) -> Result<T, Error> {
        match event {
            Recorded::IpcRecvSucc { sender_thread } => Ok(self.replay_recv(&sender_thread)),
            Recorded::IpcRecvErr(e) => Err(Box::new(ErrorKind::Custom("TODO".to_string()))),
            _ => panic!("Unexpected event: {:?} in replay for recv()",),
        }
    }

    fn replay_recv(&self, sender: &DetThreadId) -> T {
        record_replay::replay_recv(sender, self.buffer.borrow_mut(), || self.receiver.recv())
    }
}

impl<T> IpcReceiver<T>
    where T: for<'de> Deserialize<'de> + Serialize
{
    pub fn recv(&self) -> Result<T, Error> {
        self.record_replay(&self.metadata, || self.receiver.recv())
    }

    pub fn try_recv(&self) -> Result<T, Error> {
        self.record_replay(&self.metadata, || self.receiver.try_recv())
    }

    pub fn to_opaque(self) -> OpaqueIpcReceiver {
        unimplemented!()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpcSender<T>
{
    sender: ipc::IpcSender<(DetThreadId, T)>,
    /// Unique identifier assigned to every channel. Deterministic and unique
    /// even with racing thread creation. DetThreadId refers to the original
    /// creator of this thread.
    /// The partner Receiver and Sender shares the same id.
    pub(crate) id: (DetThreadId, u32),
}

impl<T> Clone for IpcSender<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    fn clone(&self) -> Self {
        IpcSender {
            sender: self.sender.clone(),
            id: self.id.clone(),
        }
    }
}

impl<T> IpcSender<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    /// Send our det thread id along with the actual message for both
    /// record and replay.
    pub fn send(&self, data: T) -> Result<(), Error> {
        log_trace(&format!("Sender<{:?}>::send()", self.id));
        // We send the det_id even when running in RecordReplayMode::NoRR,
        // but that's okay. It makes logic a little simpler.
        self.sender.send((get_det_id(), data))
    }
}

pub fn channel<T>() -> Result<(IpcSender<T>, IpcReceiver<T>), Error>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    *ENV_LOGGER;

    let (sender, receiver) = ipc::channel()?;
    let id = (get_det_id(), get_and_inc_channel_id());
    Ok((
        IpcSender {
            sender,
            id: id.clone(),
        },
        IpcReceiver::new(receiver, id),
    ))
}

/// OMAR: Wrapper necessary to change use our own IpcReceiver type for add.
/// TODO Will probably need to determize select operation.
pub struct IpcReceiverSet {
    receiver_set: ipc_channel::ipc::IpcReceiverSet,
}

impl IpcReceiverSet {
    pub fn new() -> Result<IpcReceiverSet, std::io::Error> {
        ipc_channel::ipc::IpcReceiverSet::new().map(|r| IpcReceiverSet { receiver_set: r})
    }

    pub fn add<T>(&mut self, receiver: IpcReceiver<T>) -> Result<u64, std::io::Error>
    where T: for<'de> Deserialize<'de> + Serialize {
        self.receiver_set.add(receiver.receiver)
    }

    pub fn add_opaque(&mut self, receiver: OpaqueIpcReceiver) -> Result<u64, std::io::Error> {
        self.receiver_set.add_opaque(receiver)
    }

    pub fn select(&mut self) -> Result<Vec<IpcSelectionResult>,Error> {
        unimplemented!()
    }
}
