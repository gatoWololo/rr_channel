use ipc_channel::ipc::{self};
use ipc_channel::{Error, ErrorKind};
use serde::Deserialize;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::hash_map::HashMap;
use crate::thread::DetThreadId;
use std::collections::VecDeque;
use crate::RecordReplayMode;
use crate::record_replay::{self, RecordedEvent, RecordReplay, IpcDummyError};
use crate::record_replay::RecordMetadata;

#[derive(Debug)]
pub struct IpcReceiver<T>
where
    T: for<'de> Deserialize<'de> + Serialize, {

    receiver: ipc::IpcReceiver<(DetThreadId, T)>,
    buffer: RefCell<HashMap<DetThreadId, VecDeque<T>>>,
    metadata: RecordMetadata,
}

impl<T> RecordReplay<T, Error> for IpcReceiver<T>
    where T: for<'de> Deserialize<'de> + Serialize {
    fn to_recorded_event(&self, event: Result<(DetThreadId, T), Error>) ->
        (Result<T, Error>, RecordedEvent) {
            match event {
                Ok((sender_thread, msg)) =>
                    (Ok(msg), RecordedEvent::IpcRecvSucc { sender_thread }),
                Err(e) =>
                    (Err(e), RecordedEvent::IpcRecvErr(IpcDummyError)),
            }
        }

    fn expected_recorded_events(&self, event: RecordedEvent) -> Result<T, Error> {
        match event {
            RecordedEvent::IpcRecvSucc {sender_thread} =>
                Ok(self.replay_recv(&sender_thread)),
            RecordedEvent::IpcRecvErr(e) =>
                Err(Box::new(ErrorKind::Custom("TODO".to_string()))),
            _ => panic!("Unexpected event: {:?} in replay for recv()",)
        }
    }

    fn replay_recv(&self, sender: &DetThreadId) -> T {
        record_replay::replay_recv(sender, self.buffer.borrow_mut(),
                                   || self.receiver.recv())
    }
}

impl<T> IpcReceiver<T> where
    T: for<'de> Deserialize<'de> + Serialize, {
    pub fn recv(&self) -> Result<T, Error> {
        self.record_replay(&self.metadata, || self.receiver.recv())
    }

    pub fn try_recv(&self) -> Result<T, Error> {
        self.record_replay(&self.metadata, || self.receiver.try_recv())
    }

    // pub fn to_opaque(self) -> OpaqueIpcReceiver
}
