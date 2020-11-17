// Reexport types via ipc::TYPE.

pub mod router;

pub use ipc_channel::Error;


pub mod ipc {
    use super::Error;
    use log::Level::{Warn, Info, Debug, Trace};

    use ipc_channel::ipc as ripc; //ripc = real ipc.
    pub use ipc_channel::ipc::{
        bytes_channel, IpcBytesReceiver, IpcBytesSender, IpcOneShotServer, IpcSharedMemory, TryRecvError,
    };

    pub use ipc_channel::ipc::IpcError;

    use ipc_channel::platform::{OsIpcSharedMemory, OsOpaqueIpcChannel};

    use serde::{Deserialize, Serialize};
    use std::cell::RefMut;
    use std::time::Duration;

    use ipc_channel::ipc::OpaqueIpcSender;
    use std::cell::RefCell;
    use std::collections::{hash_map::HashMap, VecDeque};

    use std::thread;

    use crate::desync::DesyncMode;
    use crate::detthread::DetThreadId;
    use crate::error::{DesyncError, RecvErrorRR};
    use crate::recordlog::{RecordedEvent, IpcErrorVariants, Recordable, RecordMetadata};
    use crate::recordlog::{self, ChannelLabel, IpcSelectEvent};
    use crate::rr::RecvRecordReplay;
    use crate::rr::SendRecordReplay;
    use crate::rr::{self, DetChannelId};
    use crate::{desync, detthread, InMemoryRecorder, EventRecorder, log_rr};
    use crate::{get_generic_name, RRMode, RECORD_MODE, DetMessage, DESYNC_MODE, ENV_LOGGER, BufferedValues};
    use std::io::ErrorKind;

    type OsIpcReceiverResults = (Vec<u8>, Vec<OsOpaqueIpcChannel>, Vec<OsIpcSharedMemory>);

    #[derive(Serialize, Deserialize)]
    pub struct IpcReceiver<T> {
        pub(crate) receiver: ripc::IpcReceiver<DetMessage<T>>,
        buffer: RefCell<BufferedValues<T>>,
        pub(crate) metadata: recordlog::RecordMetadata,
        event_recorder: EventRecorder,
    }

    impl<T> IpcReceiver<T> {
        pub(crate) fn new(
            receiver: ripc::IpcReceiver<DetMessage<T>>,
            id: DetChannelId, recorder: EventRecorder,
        ) -> IpcReceiver<T> {
            IpcReceiver {
                receiver,
                buffer: RefCell::new(HashMap::new()),
                metadata: recordlog::RecordMetadata {
                    type_name: get_generic_name::<T>().to_string(),
                    flavor: ChannelLabel::Ipc,
                    mode: *RECORD_MODE,
                    id,
                },
                event_recorder: recorder,
            }
        }


        fn ipc_error_to_recorded_event(e: &IpcError) -> IpcErrorVariants {
            match e {
                IpcError::Bincode(_e) => {
                    IpcErrorVariants::Bincode
                }
                IpcError::Io(_e) => {
                    IpcErrorVariants::Io
                }
                IpcError::Disconnected => {
                    IpcErrorVariants::Disconnected
                }
            }
        }

        fn recorded_event_to_ipc_error(re: IpcErrorVariants) -> IpcError {
            match re {
                IpcErrorVariants::Disconnected => {
                    ipc_channel::ipc::IpcError::Disconnected
                }
                IpcErrorVariants::Io => {
                    let err = "Unknown IpcError::IO";
                    ipc_channel::ipc::IpcError::Io(std::io::Error::new(ErrorKind::Other, err))
                }
                IpcErrorVariants::Bincode => {
                    let err = "Unknown IpcError::Bincode".to_string();
                    ipc_channel::ipc::IpcError::Bincode(Box::new(ipc_channel::ErrorKind::Custom(err)))
                }
            }
        }
    }

    impl<T> rr::RecvRecordReplay<T, ipc_channel::ipc::IpcError> for IpcReceiver<T>
        where
            T: for<'de> Deserialize<'de> + Serialize,
    {
        fn recorded_event_succ(dtid: DetThreadId) -> recordlog::RecordedEvent {
            RecordedEvent::IpcRecvSucc { sender_thread: dtid }
        }

        // This isn't ideal, but IpcError doesn't easily implement serialize/deserialize, so this
        // is our best effor attempt.
        fn recorded_event_err(e: &ipc_channel::ipc::IpcError) -> recordlog::RecordedEvent {
            RecordedEvent::IpcError(<IpcReceiver<T>>::ipc_error_to_recorded_event(e))
        }

        fn replay_recorded_event(
            &self,
            event: RecordedEvent,
        ) -> desync::Result<Result<T, ipc_channel::ipc::IpcError>> {
            match event {
                RecordedEvent::IpcRecvSucc { sender_thread } => {
                    let retval = self.replay_recv(&sender_thread)?;
                    detthread::inc_event_id();
                    Ok(Ok(retval))
                }
                RecordedEvent::IpcError(variant) => {
                    detthread::inc_event_id();
                    Ok(Err(<IpcReceiver<T>>::recorded_event_to_ipc_error(variant)))
                }
                e => {
                    let mock_event = RecordedEvent::IpcRecvSucc {
                        // TODO: Is there a better value to show this is a mock DTI?
                        sender_thread: DetThreadId::new(),
                    };
                    Err(DesyncError::EventMismatch(e, mock_event))
                }
            }
        }
    }


    impl<T> rr::RecvRecordReplay<T, ipc_channel::ipc::TryRecvError> for IpcReceiver<T>
        where
            T: for<'de> Deserialize<'de> + Serialize,
    {
        fn recorded_event_succ(dtid: DetThreadId) -> recordlog::RecordedEvent {
            RecordedEvent::IpcRecvSucc { sender_thread: dtid }
        }

        fn recorded_event_err(e: &ipc_channel::ipc::TryRecvError) -> recordlog::RecordedEvent {
            match e {
                TryRecvError::Empty => {
                    RecordedEvent::IpcTryRecvErrorEmpty
                }
                TryRecvError::IpcError(e) => {
                    RecordedEvent::IpcTryRecvIpcError(<IpcReceiver<T>>::ipc_error_to_recorded_event(e))
                }
            }
        }

        fn replay_recorded_event(
            &self,
            event: RecordedEvent,
        ) -> desync::Result<Result<T, ipc_channel::ipc::TryRecvError>> {
            match event {
                RecordedEvent::IpcRecvSucc { sender_thread } => {
                    let retval = self.replay_recv(&sender_thread)?;
                    detthread::inc_event_id();
                    Ok(Ok(retval))
                }
                RecordedEvent::IpcTryRecvIpcError(variant) => {
                    detthread::inc_event_id();
                    Ok(Err(TryRecvError::IpcError(<IpcReceiver<T>>::recorded_event_to_ipc_error(variant))))
                }
                RecordedEvent::IpcTryRecvErrorEmpty => {
                    detthread::inc_event_id();
                    Ok(Err(TryRecvError::Empty))
                }
                e => {
                    let mock_event = RecordedEvent::IpcRecvSucc {
                        // TODO: Is there a better value to show this is a mock DTI?
                        sender_thread: DetThreadId::new(),
                    };
                    Err(DesyncError::EventMismatch(e, mock_event))
                }
            }
        }
    }

    pub struct OpaqueIpcReceiver {
        opaque_receiver: ipc_channel::ipc::OpaqueIpcReceiver,
        pub(crate) metadata: recordlog::RecordMetadata,
        /// For the IpceReceiverSet to work correctly, we keep track of when
         /// a OpaqueIpcReceiver is closed, to avoid adding it to the set
        /// of receivers when we desync. We track this information here.
        /// See: https://github.com/gatoWololo/rr_channel/issues/28
        closed: bool,
    }

    impl<T> IpcReceiver<T>
        where T: for<'de> Deserialize<'de> + Serialize {
        fn rr_try_recv(&self) -> Result<DetMessage<T>, RecvErrorRR> {
            for _ in 0..10 {
                match self.receiver.try_recv() {
                    Ok(v) => return Ok(v),
                    Err(_) => {
                        // TODO FIX!!!!
                        // Divide timeout by power of two 1 million
                        // power of two.
                        thread::sleep(Duration::from_millis(100))
                    }
                }
            }
            Err(RecvErrorRR::Timeout)
        }

        /// Deterministic receiver which loops until event comes from `sender`.
        /// All other entries are buffer is `self.buffer`. Buffer is always
        /// checked before entries.
        fn replay_recv(&self, sender: &DetThreadId) -> desync::Result<T> {
            rr::recv_expected_message(
                sender,
                || self.rr_try_recv(),
                &mut self.get_buffer(),
                // &self.metadata.id,
            )
        }

        pub fn recv(&self) -> Result<T, ipc_channel::ipc::IpcError> {
            let f = || self.receiver.recv();
            let recorder = self.event_recorder.get_recordable();
            self.record_replay_with(&self.metadata, f, recorder)
                .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
        }

        pub fn try_recv(&self) -> Result<T, TryRecvError> {
            let f = || self.receiver.try_recv();
            let recorder = self.event_recorder.get_recordable();
            let foo = self.record_replay_with(&self.metadata, f, recorder);
            foo.unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
        }

        pub fn to_opaque(self) -> OpaqueIpcReceiver {
            let metadata = self.metadata;
            OpaqueIpcReceiver {
                opaque_receiver: self.receiver.to_opaque(),
                metadata,
                closed: false,
            }
        }

        pub(crate) fn get_buffer(&self) -> RefMut<BufferedValues<T>> {
            self.buffer.borrow_mut()
        }
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct IpcSender<T> {
        sender: ripc::IpcSender<DetMessage<T>>,
        pub(crate) metadata: recordlog::RecordMetadata,
        recorder: EventRecorder,
    }

    /// We derive our own instance of Clone to avoid the Constraint that T: Clone
    impl<T> Clone for IpcSender<T>
        where
            T: Serialize,
    {
        fn clone(&self) -> Self {
            IpcSender {
                sender: self.sender.clone(),
                metadata: self.metadata.clone(),
                recorder: self.recorder.clone(),
            }
        }
    }

    impl<T> rr::SendRecordReplay<T, Error> for IpcSender<T>
        where
            T: Serialize,
    {
        const EVENT_VARIANT: RecordedEvent = RecordedEvent::IpcSender;

        fn check_log_entry(&self, entry: RecordedEvent) -> desync::Result<()> {
            match entry {
                RecordedEvent::IpcSender => Ok(()),
                event_entry => Err(DesyncError::EventMismatch(
                    event_entry,
                    RecordedEvent::IpcSender,
                )),
            }
        }

        fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), Error> {
            self.sender.send((thread_id, msg))
        }
    }

    impl<T> IpcSender<T>
        where
            T: Serialize,
    {
        pub(crate) fn new(sender: ripc::IpcSender<(DetThreadId, T)>,
                          metadata: RecordMetadata,
                          recorder: EventRecorder) -> IpcSender<T> {
            IpcSender {
                sender,
                metadata,
                recorder,
            }
        }
        /// Send our det thread id along with the actual message for both
        /// record and replay.
        pub fn send(&self, data: T) -> Result<(), ipc_channel::Error> {
            match self.record_replay_send(
                data,
                &self.metadata,
                self.recorder.get_recordable()
            ) {
                Ok(v) => v,
                Err((error, msg)) => {
                    log_rr!(Warn, "IpcSend::Desynchronization detected: {:?}", error);
                    match *DESYNC_MODE {
                        DesyncMode::Panic => panic!("IpcSend::Desynchronization detected: {:?}", error),
                        // TODO: One day we may want to record this alternate execution.
                        DesyncMode::KeepGoing => {
                            desync::mark_program_as_desynced();
                            let res = rr::SendRecordReplay::underlying_send(self, detthread::get_forwarding_id(), msg);
                            // TODO Ugh, right now we have to carefully increase the event_id
                            // in the "right places" or nothing will work correctly.
                            // How can we make this a lot less error prone?
                            detthread::inc_event_id();
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

            let type_name = get_generic_name::<T>();
            log_rr!(Info, "Sender connected created: {:?} {:?}", id, type_name);

            let metadata = recordlog::RecordMetadata {
                type_name: type_name.to_string(),
                flavor: ChannelLabel::Ipc,
                mode: *RECORD_MODE,
                id,
            };
            ipc_channel::ipc::IpcSender::connect(name).map(|sender| IpcSender { sender, metadata, recorder: todo!()  })
        }
    }

    /// Part of IPC public interface.
    pub fn channel<T>() -> Result<(IpcSender<T>, IpcReceiver<T>), std::io::Error>
        where
            T: for<'de> Deserialize<'de> + Serialize,
    {
        spawn_ipc_channels(EventRecorder::new_file_recorder(), *RECORD_MODE)
    }

    /// Allows us to test IPC channels using a inmemory recorder.
    pub(crate) fn in_memory_channel<T>(rr_mode: RRMode) -> Result<(IpcSender<T>, IpcReceiver<T>), std::io::Error>
        where
            T: for<'de> Deserialize<'de> + Serialize,
    {
        spawn_ipc_channels(EventRecorder::new_memory_recorder(), rr_mode)
    }

    /// Encapsulate all logic for properly setting up channels. Called by channel() function. Useful
    /// for testing channels directly.
    fn spawn_ipc_channels<T>(recorder: EventRecorder, record_mode: RRMode)
        -> Result<(IpcSender<T>, IpcReceiver<T>), std::io::Error>
        where
            T: for<'de> Deserialize<'de> + Serialize,
    {
        let (sender, receiver) = ripc::channel()?;
        let id = DetChannelId::new();
        let type_name = get_generic_name::<T>();
        log_rr!(Info, "IPC channel created: {:?} {:?}", id, type_name);

        let metadata = recordlog::RecordMetadata {
            type_name: type_name.to_string(),
            flavor: ChannelLabel::Ipc,
            mode: record_mode,
            id: id.clone(),
        };

        Ok((
            IpcSender::new(sender, metadata, recorder.clone()),
            IpcReceiver::new(receiver, id, recorder),
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
        pub fn to<T>(self) -> Result<T, bincode::Error>
            where
                T: for<'de> Deserialize<'de> + Serialize,
        {
            self.opaque.to::<DetMessage<T>>().map(|(_, v)| v)
        }
    }

    /// We assume that receivers will be added in deterministic order. We wrap the
    /// router to ensure this is true for `RouterProxy`'s use of `IpcReceiverSet`.
    /// If receivers are added in different order, this will cause replay to fail.
    /// panic!()/warn!()
    pub struct IpcReceiverSet {
        receiver_set: ipc_channel::ipc::IpcReceiverSet,
        mode: RRMode,
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
        buffer: HashMap<u64, HashMap<DetThreadId, VecDeque<OpaqueIpcMessage>>>,
        /// This `IpcReceiverSet` has become desynchronized. Any call to methods that
        /// return `DesyncError`s will return an error.
        desynced: bool,
        /// Holds on to senders counterparts for "dummy" receivers that have already
        /// been closed and then we desynchronized. We need to hang on to these
        /// receivers, but NEVER use them, have the "dummy" receivers act like
        /// closed receivers. We add sendes in abitrary order and nothing should be
        /// assumed about their ordering.
        /// See https://github.com/gatoWololo/rr_channel/issues/28
        dummy_senders: Vec<OpaqueIpcSender>,
        /// Represents the recorder of events.
        recorder: EventRecorder,
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
                recorder: EventRecorder::new_file_recorder()
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
            self.rr_add(receiver).unwrap_or_else(|(e, receiver)| {
                log_rr!(Warn, "IpcReceiver::add::Desynchonization detected: {:?}", e);
                match *DESYNC_MODE {
                    DesyncMode::Panic => {
                        panic!("IpcReceiver::add::Desynchronization detected: {:?}", e)
                    }
                    DesyncMode::KeepGoing => {
                        desync::mark_program_as_desynced();
                        self.move_receivers_to_set();
                        detthread::inc_event_id();
                        self.receiver_set.add_opaque(receiver.opaque_receiver)
                    }
                }
            })
        }

        pub fn select(&mut self) -> Result<Vec<IpcSelectionResult>, std::io::Error> {
            log_rr!(Info, "IpcSelect::select()");
            self.rr_select().unwrap_or_else(|(e, _entries)| {
                log_rr!(Warn, "IpcSelect::Desynchonization detected: {:?}", e);

                match *DESYNC_MODE {
                    DesyncMode::Panic => {
                        panic!("IpcSelect::Desynchronization detected: {:?}", e);
                    }
                    DesyncMode::KeepGoing => {
                        desync::mark_program_as_desynced();
                        log_rr!(
                        Trace,
                        "Looking for entries in buffer or call to select()..."
                    );

                        self.move_receivers_to_set();
                        // First use up our buffered entries, if none. Do the
                        // actual wait.
                        match self.get_buffered_entries() {
                            None => {
                                log_rr!(Debug, "Doing direct do_select()");
                                detthread::inc_event_id();
                                self.do_select()
                            }
                            Some(be) => {
                                log_rr!(Debug, "Using buffered entries.");
                                detthread::inc_event_id();
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
        fn rr_select(
            &mut self,
        ) -> Result<
            Result<Vec<IpcSelectionResult>, std::io::Error>,
            (DesyncError, Vec<IpcSelectionResult>),
        > {
            if desync::program_desyned() {
                return Err((DesyncError::Desynchronized, vec![]));
            }
            match self.mode {
                RRMode::Record => {
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
                                recorded_events.push(IpcSelectEvent::MessageReceived(index, det_id));
                            }
                            IpcSelectionResult::ChannelClosed(index) => {
                                moved_events.push(IpcSelectionResult::ChannelClosed(index));
                                recorded_events.push(IpcSelectEvent::ChannelClosed(index));
                            }
                        }
                    }

                    let event = RecordedEvent::IpcSelect {
                        select_events: recorded_events,
                    };
                    // Ehh, we fake it here. We never check this value anyways.
                    self.recorder.get_recordable().write_event_to_record(
                        event,
                        &recordlog::RecordMetadata::new("IpcSelect".to_string(),
                                                        ChannelLabel::IpcSelect,
                                                        self.mode,
                                                        DetChannelId::fake())

                    );
                    Ok(Ok(moved_events))
                }

                // Use index to fetch correct receiver and read value directly from
                // receiver.
                RRMode::Replay => {
                    if self.desynced {
                        return Err((DesyncError::Desynchronized, vec![]));
                    }
                    let det_id = detthread::get_det_id();

                    // Here we put this thread to sleep if the entry is missing.
                    // On record, the thread never returned from blocking...
                    let entry = self.recorder.get_recordable().get_log_entry(det_id, detthread::get_event_id());

                    if let Err(e @ DesyncError::NoEntryInLog(_, _)) = entry {
                        log_rr!(Info, "Saw {:?}. Putting thread to sleep.", e);
                        desync::sleep_until_desync();
                        // Thread woke back up... desynced!
                        return Err((DesyncError::DesynchronizedWakeup, vec![]));
                    }

                    match entry.map_err(|e| (e, vec![]))? {
                        RecordedEvent::IpcSelect { select_events } => {
                            let mut events: Vec<IpcSelectionResult> = Vec::new();

                            for event in select_events {
                                match self.replay_select_event(&event) {
                                    Err((error, entry)) => {
                                        if let Some(e) = entry {
                                            events.push(e);
                                        }
                                        return Err((error, events));
                                    }
                                    Ok(entry) => events.push(entry),
                                }
                            }

                            detthread::inc_event_id();
                            Ok(Ok(events))
                        }
                        event => {
                            let dummy = RecordedEvent::IpcSelect {
                                select_events: vec![],
                            };
                            Err((DesyncError::EventMismatch(event.clone(), dummy), vec![]))
                        }
                    }
                }
                RRMode::NoRR => Ok(self.do_select()),
            }
        }

        /// From a IpcSelectEvent, fetch the correct IpcSelectionResult.
        /// On error return the event that caused Desync.
        fn replay_select_event(
            &mut self,
            event: &IpcSelectEvent,
        ) -> Result<IpcSelectionResult, (DesyncError, Option<IpcSelectionResult>)> {
            log_rr!(Trace, "replay_select_event for {:?}", event);
            match event {
                IpcSelectEvent::MessageReceived(index, expected_sender) => {
                    let receiver = self.receivers.get_mut(*index as usize).
                        // This should never happen. The way the code is written, by the
                        // time we get here, all receivers have been registered with us correctly.
                        // If any of them are missing. We would have seen it already.
                        // expect here represents a bug on the internal assumptions of our code.
                        expect("Unable to fetch correct receiver from map.");

                    // Auto buffers results.
                    Ok(IpcReceiverSet::do_replay_recv_for_entry(
                        &mut self.buffer,
                        *index,
                        expected_sender,
                        receiver,
                    )?)
                }
                IpcSelectEvent::ChannelClosed(index) => {
                    let receiver = self.receivers.get_mut(*index as usize).
                        // This should never happen. The way the code is written, by the
                        // time we get here, all receivers have been registered with us correctly.
                        // If any of them are missing. We would have seen it already.
                        // expect here represents a bug on the internal assumptions of our code.
                        expect("Unable to fetch correct receiver from map.");

                    match IpcReceiverSet::rr_recv(receiver) {
                        Err(RecvErrorRR::Disconnected) => {
                            log_rr!(
                            Trace,
                            "replay_select_event(): Saw channel closed for {:?}",
                            index
                        );
                            // remember that this receiver closed!
                            receiver.closed = true;
                            Ok(IpcSelectionResult::ChannelClosed(*index))
                        }
                        Err(RecvErrorRR::Timeout) => {
                            return Err((DesyncError::Timedout, None));
                        }
                        Ok(val) => {
                            // Expected a channel closed, saw a real message...
                            log_rr!(Trace, "Expected channel closed! Saw success: {:?}", index);
                            Err((
                                DesyncError::ChannelClosedExpected,
                                Some(IpcReceiverSet::convert(*index, val.0, val.1, val.2)),
                            ))
                        }
                    }
                }
            }
        }

        fn rr_recv(
            opaque_receiver: &mut OpaqueIpcReceiver,
        ) -> Result<OsIpcReceiverResults, RecvErrorRR> {
            let receiver = &mut opaque_receiver.opaque_receiver.os_receiver;
            for _ in 0..10 {
                match receiver.try_recv() {
                    Ok(v) => return Ok(v),
                    Err(e) if e.channel_is_closed() => return Err(RecvErrorRR::Disconnected),
                    Err(_) => thread::sleep(Duration::from_millis(100)),
                }
            }
            Err(RecvErrorRR::Timeout)
        }

        /// TODO: There is borrowing issues if I mut borrow the receiver, and
        /// have this function be a method with (&mut self). So I pass everything
        /// explicitly for now.

        /// Loop until correct message is found for passed receiver. If the message
        /// is from the "wrong" sender, it is buffered.
        /// DesyncError is returned if the channel closes on us. We return the
        /// Channel closed event.
        fn do_replay_recv_for_entry(
            buffer: &mut HashMap<u64, HashMap<DetThreadId, VecDeque<OpaqueIpcMessage>>>,
            index: u64,
            expected_sender: &DetThreadId,
            receiver: &mut OpaqueIpcReceiver,
        ) -> Result<IpcSelectionResult, (DesyncError, Option<IpcSelectionResult>)> {
            log_rr!(Debug, "do_replay_recv_for_entry");

            if let Some(entry) = buffer
                .get_mut(&index)
                .and_then(|m| m.get_mut(expected_sender))
                .and_then(|e| e.pop_front())
            {
                log_rr!(Trace, "IpcSelect(): Recv message found in buffer.");
                return Ok(IpcSelectionResult::MessageReceived(index, entry));
            }

            loop {
                let (data, channels, shared_memory_regions) =
                    // TODO timeout here!
                    match IpcReceiverSet::rr_recv(receiver) {
                        Ok(v) => v,
                        Err(RecvErrorRR::Disconnected) => {
                            log_rr!(Trace, "Expected message, saw channel closed.");
                            // remember that this receiver closed!
                            receiver.closed = true;
                            return Err((DesyncError::ChannelClosedUnexpected(index),
                                        Some(IpcSelectionResult::ChannelClosed(index))));
                        }
                        Err(RecvErrorRR::Timeout) => {
                            return Err((DesyncError::Timedout, None));
                        }
                    };

                let msg = OpaqueIpcMessage::new(data, channels, shared_memory_regions);
                let det_id = IpcReceiverSet::deserialize_id(&msg);

                if &det_id == expected_sender {
                    log_rr!(Trace, "do_replay_recv_for_entry: message found via recv()");
                    return Ok(IpcSelectionResult::MessageReceived(index, msg));
                } else {
                    log_rr!(
                    Trace,
                    "Wrong message received from {:?}, adding it to buffer",
                    det_id
                );
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
            let selected: Vec<IpcSelectionResult> = selected
                .into_iter()
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
        fn rr_add(
            &mut self,
            receiver: OpaqueIpcReceiver,
        ) -> Result<Result<u64, std::io::Error>, (DesyncError, OpaqueIpcReceiver)> {
            if desync::program_desyned() {
                return Err((DesyncError::Desynchronized, receiver));
            }
            let metadata = receiver.metadata.clone();
            let flavor = metadata.flavor;
            let id = &metadata.id;
            log_rr!(Debug, "IpcSelect::rr_add<{:?}>()", id);

            // After the first time we desynchronize, this is set to true and future times
            // we eill be sent here.
            if self.desynced {
                return Err((DesyncError::Desynchronized, receiver));
            }

            match self.mode {
                RRMode::Record => {
                    let index = match self.receiver_set.add_opaque(receiver.opaque_receiver) {
                        Ok(v) => v,
                        e @ Err(_) => return Ok(e),
                    };

                    let event = RecordedEvent::IpcSelectAdd(index);
                    self.recorder.get_recordable().write_event_to_record(event, &metadata);
                    Ok(Ok(index))
                }
                // Do not add receiver to IpcReceiverSet, instead move the receiver
                // to our own `receivers` hashmap where the index returned here is
                // the key.
                RRMode::Replay => {
                    let det_id = detthread::get_det_id();

                    let log_entry = match self.recorder.get_recordable().get_log_entry_with(
                        det_id,
                        detthread::get_event_id(),
                        &flavor,
                        &id,
                    ) {
                        Err(e) => return Err((e, receiver)),
                        Ok(v) => v,
                    };
                    let index = self.index as u64;
                    match log_entry {
                        RecordedEvent::IpcSelectAdd(r_index) => {
                            if r_index != index {
                                let error = DesyncError::SelectIndexMismatch(r_index, index);
                                return Err((error, receiver));
                            }
                            if let Some(_) = self.receivers.get(self.index) {
                                let error = DesyncError::SelectExistingEntry(index);
                                return Err((error, receiver));
                            }

                            self.receivers.insert(self.index, receiver);

                            self.index += 1;
                            detthread::inc_event_id();
                            Ok(Ok(index))
                        }
                        event => {
                            let dummy = RecordedEvent::IpcSelectAdd((0 - 1) as u64);
                            Err((DesyncError::EventMismatch(event.clone(), dummy), receiver))
                        }
                    }
                }
                RRMode::NoRR => Ok(self.receiver_set.add_opaque(receiver.opaque_receiver)),
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
            if !self.desynced {
                log_rr!(Debug, "Moving all receivers into actual select set.");

                // IMPORTANT: Vec ensures that element are drained in order they were added!
                // This is necessary for the receiver set indices to match.
                for r in self.receivers.drain(..) {
                    if r.closed {
                        // This receiver has closed. Use dummy receiver to avoid duplicating
                        // closed message.

                        // We don't know the correct type of this message, but that is
                        // okay. We will never read from this receiver.
                        let (s, r) =
                            channel::<()>().expect("Unable to create channels for dummy receiver");
                        // keep sender around forever to avoid dropping channel.
                        self.dummy_senders.push(s.to_opaque());
                        let index = self
                            .receiver_set
                            .add_opaque(r.to_opaque().opaque_receiver)
                            .expect(
                                "Unable to add dummy receiver while \
                                handling desynchronization.",
                            );
                        log_rr!(Trace, "Adding dummy receiver at index {:?}", index);
                    } else {
                        // Add the real receiver.
                        let id = r.metadata.id.clone();
                        let i = self
                            .receiver_set
                            .add_opaque(r.opaque_receiver)
                            .expect("Unable to add receiver while handling desynchronization.");

                        log_rr!(
                        Trace,
                        "move_receivers_to_set: Added receiver {:?} with index: {:?}",
                        id,
                        i
                    );
                    }
                }
                self.desynced = true;
            }
        }

        /// Extract DetThreadId from opaque message. Not easy :P
        fn deserialize_id(opaque_msg: &OpaqueIpcMessage) -> DetThreadId {
            let (det_id, _): (DetThreadId, /*Unknown T*/ ()) =
                bincode::deserialize(&opaque_msg.opaque.data)
                    .expect("Unable to deserialize DetThreadId");
            det_id
        }

        fn convert(
            index: u64,
            data: Vec<u8>,
            os_ipc_channels: Vec<OsOpaqueIpcChannel>,
            os_ipc_shared_memory_regions: Vec<OsIpcSharedMemory>,
        ) -> IpcSelectionResult {
            let msg = OpaqueIpcMessage::new(data, os_ipc_channels, os_ipc_shared_memory_regions);
            IpcSelectionResult::MessageReceived(index, msg)
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{init_tivo_thread_root, RRMode};
    use crate::ipc_channel::ipc;
    use ipc_channel::ipc::IpcError;

    #[test]
    fn ipc_channel() {
        init_tivo_thread_root();
        let (s, r) = ipc::in_memory_channel::<i32>(RRMode::Replay).unwrap();

        s.send(5).unwrap();
        let v = r.recv().unwrap();
        assert_eq!(v, 5);
    }
}