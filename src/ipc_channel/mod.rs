// Reexport types via ipc::TYPE.

pub mod router;
pub use ipc_channel::Error;

pub mod ipc {
    use crate::{fn_basename, get_det_id};
    use ipc_channel::Error;

    #[allow(unused_imports)]
    use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

    use ipc_channel::ipc as ripc; //ripc = real ipc.
    pub use ipc_channel::ipc::{
        bytes_channel, IpcBytesReceiver, IpcBytesSender, IpcOneShotServer, IpcSharedMemory,
        TryRecvError,
    };

    pub use ipc_channel::ipc::IpcError;

    use ipc_channel::platform::{OsIpcSharedMemory, OsOpaqueIpcChannel};

    use serde::{Deserialize, Serialize};
    use std::cell::RefMut;
    use std::time::Duration;

    use ipc_channel::ipc::OpaqueIpcSender;
    use std::cell::RefCell;
    use std::collections::{hash_map::HashMap, VecDeque};
    use std::{io, thread};

    use crate::desync::DesyncMode;
    use crate::detthread::DetThreadId;
    use crate::error::{DesyncError, RecvErrorRR};
    use crate::recordlog::{self, ChannelVariant, IpcSelectEvent, RecordEntry};
    use crate::recordlog::{IpcErrorVariants, RecordMetadata, RecordedEvent};
    use crate::rr::SendRecordReplay;
    use crate::rr::{self, DetChannelId};
    use crate::rr::{RecordEventChecker, RecvRecordReplay};
    use crate::{desync, get_rr_mode, EventRecorder};
    use crate::{BufferedValues, DetMessage, RRMode, DESYNC_MODE};
    use std::any::type_name;
    use std::io::ErrorKind;

    type OsIpcReceiverResults = (Vec<u8>, Vec<OsOpaqueIpcChannel>, Vec<OsIpcSharedMemory>);

    #[derive(Serialize, Deserialize, Debug)]
    pub struct IpcReceiver<T> {
        pub(crate) receiver: ripc::IpcReceiver<DetMessage<T>>,
        buffer: RefCell<BufferedValues<T>>,
        pub(crate) metadata: recordlog::RecordMetadata,
        event_recorder: EventRecorder,
        mode: RRMode,
    }

    impl<T> IpcReceiver<T>
    where
        T: for<'de> Deserialize<'de> + Serialize,
    {
        pub(crate) fn new(
            receiver: ripc::IpcReceiver<DetMessage<T>>,
            id: DetChannelId,
            recorder: EventRecorder,
            mode: RRMode,
        ) -> IpcReceiver<T> {
            info!("{}", crate::function_name!());
            IpcReceiver {
                receiver,
                buffer: RefCell::new(HashMap::new()),
                metadata: recordlog::RecordMetadata {
                    type_name: type_name::<T>().to_string(),
                    channel_variant: ChannelVariant::Ipc,
                    id,
                },
                event_recorder: recorder,
                mode,
            }
        }

        fn ipc_error_to_recorded_event(e: &IpcError) -> IpcErrorVariants {
            match e {
                IpcError::Bincode(_e) => IpcErrorVariants::Bincode,
                IpcError::Io(_e) => IpcErrorVariants::Io,
                IpcError::Disconnected => IpcErrorVariants::Disconnected,
            }
        }

        fn recorded_event_to_ipc_error(re: IpcErrorVariants) -> IpcError {
            match re {
                IpcErrorVariants::Disconnected => ipc_channel::ipc::IpcError::Disconnected,
                IpcErrorVariants::Io => {
                    let err = "Unknown IpcError::IO";
                    IpcError::Io(std::io::Error::new(ErrorKind::Other, err))
                }
                IpcErrorVariants::Bincode => {
                    let err = "Unknown IpcError::Bincode".to_string();
                    IpcError::Bincode(Box::new(ipc_channel::ErrorKind::Custom(err)))
                }
            }
        }

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
            rr::recv_expected_message(sender, || self.rr_try_recv(), &mut self.get_buffer())
        }

        pub fn recv(&self) -> Result<T, ipc_channel::ipc::IpcError> {
            let f = || self.receiver.recv();
            self.record_replay(f)
                .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
        }

        pub fn try_recv(&self) -> Result<T, TryRecvError> {
            let f = || self.receiver.try_recv();
            self.record_replay(f)
                .unwrap_or_else(|e| desync::handle_desync(e, f, self.get_buffer()))
        }

        pub fn to_opaque(self) -> OpaqueIpcReceiver {
            let metadata = self.metadata;
            OpaqueIpcReceiver {
                opaque_receiver: self.receiver.to_opaque(),
                metadata,
                closed: false,
            }
        }

        fn record_replay<E>(
            &self,
            g: impl FnOnce() -> Result<DetMessage<T>, E>,
        ) -> desync::Result<Result<T, E>>
        where
            Self: RecvRecordReplay<T, E>,
        {
            self.record_replay_recv(&self.mode, &self.metadata, g, &self.event_recorder)
        }

        pub(crate) fn get_buffer(&self) -> RefMut<BufferedValues<T>> {
            self.buffer.borrow_mut()
        }
    }

    impl<T> RecordEventChecker<ipc_channel::ipc::IpcError> for IpcReceiver<T> {
        fn check_recorded_event(&self, re: &RecordedEvent) -> Result<(), RecordedEvent> {
            match re {
                RecordedEvent::IpcRecvSucc { sender_thread: _ } => Ok(()),
                RecordedEvent::IpcError(_) => Ok(()),
                _ => Err(RecordedEvent::IpcRecvSucc {
                    // TODO: Is there a better value to show this is a mock DTI?
                    sender_thread: DetThreadId::new(),
                }),
            }
        }
    }

    impl<T> RecvRecordReplay<T, ipc_channel::ipc::IpcError> for IpcReceiver<T>
    where
        T: for<'de> Deserialize<'de> + Serialize,
    {
        fn recorded_event_succ(dtid: DetThreadId) -> recordlog::RecordedEvent {
            RecordedEvent::IpcRecvSucc {
                sender_thread: dtid,
            }
        }

        // This isn't ideal, but IpcError doesn't easily implement serialize/deserialize, so this
        // is our best effort attempt.
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
                    Ok(Ok(retval))
                }
                RecordedEvent::IpcError(variant) => {
                    Ok(Err(<IpcReceiver<T>>::recorded_event_to_ipc_error(variant)))
                }
                _ => unreachable!(
                    "We should have already checked for this in {}",
                    stringify!(check_event_mismatch)
                ),
            }
        }
    }

    impl<T> RecordEventChecker<ipc_channel::ipc::TryRecvError> for IpcReceiver<T> {
        fn check_recorded_event(&self, re: &RecordedEvent) -> Result<(), RecordedEvent> {
            match re {
                RecordedEvent::IpcRecvSucc { sender_thread: _ } => Ok(()),
                RecordedEvent::IpcTryRecvIpcError(_) => Ok(()),
                RecordedEvent::IpcTryRecvErrorEmpty => Ok(()),
                _ => Err(RecordedEvent::IpcRecvSucc {
                    // TODO: Is there a better value to show this is a mock DTI?
                    sender_thread: DetThreadId::new(),
                }),
            }
        }
    }

    impl<T> RecvRecordReplay<T, ipc_channel::ipc::TryRecvError> for IpcReceiver<T>
    where
        T: for<'de> Deserialize<'de> + Serialize,
    {
        fn recorded_event_succ(dtid: DetThreadId) -> recordlog::RecordedEvent {
            RecordedEvent::IpcRecvSucc {
                sender_thread: dtid,
            }
        }

        fn recorded_event_err(e: &ipc_channel::ipc::TryRecvError) -> recordlog::RecordedEvent {
            match e {
                TryRecvError::Empty => RecordedEvent::IpcTryRecvErrorEmpty,
                TryRecvError::IpcError(e) => RecordedEvent::IpcTryRecvIpcError(
                    <IpcReceiver<T>>::ipc_error_to_recorded_event(e),
                ),
            }
        }

        fn replay_recorded_event(
            &self,
            event: RecordedEvent,
        ) -> desync::Result<Result<T, ipc_channel::ipc::TryRecvError>> {
            match event {
                RecordedEvent::IpcRecvSucc { sender_thread } => {
                    let retval = self.replay_recv(&sender_thread)?;
                    Ok(Ok(retval))
                }
                RecordedEvent::IpcTryRecvIpcError(variant) => Ok(Err(TryRecvError::IpcError(
                    <IpcReceiver<T>>::recorded_event_to_ipc_error(variant),
                ))),
                RecordedEvent::IpcTryRecvErrorEmpty => Ok(Err(TryRecvError::Empty)),
                _ => unreachable!(
                    "This should have already been checked in {}!",
                    stringify!(check_event_mismatch)
                ),
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

    #[derive(Debug, Serialize, Deserialize)]
    pub struct IpcSender<T> {
        sender: ripc::IpcSender<DetMessage<T>>,
        pub(crate) metadata: recordlog::RecordMetadata,
        recorder: EventRecorder,
        mode: RRMode,
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
                mode: get_rr_mode(),
            }
        }
    }

    impl<T> RecordEventChecker<Error> for IpcSender<T> {
        fn check_recorded_event(&self, re: &RecordedEvent) -> Result<(), RecordedEvent> {
            match re {
                RecordedEvent::IpcSender => Ok(()),
                _ => Err(RecordedEvent::IpcSender),
            }
        }
    }

    impl<T> SendRecordReplay<T, Error> for IpcSender<T>
    where
        T: Serialize,
    {
        const EVENT_VARIANT: RecordedEvent = RecordedEvent::IpcSender;

        fn underlying_send(&self, thread_id: DetThreadId, msg: T) -> Result<(), Error> {
            self.sender.send((thread_id, msg))
        }
    }

    impl<T> IpcSender<T>
    where
        T: Serialize,
    {
        pub(crate) fn new(
            sender: ripc::IpcSender<(DetThreadId, T)>,
            metadata: RecordMetadata,
            recorder: EventRecorder,
            mode: RRMode,
        ) -> IpcSender<T> {
            info!("{}", crate::function_name!());
            IpcSender {
                sender,
                metadata,
                recorder,
                mode,
            }
        }

        fn span(&self, fn_name: &str) -> EnteredSpan {
            span!(Level::INFO, stringify!(IpcSender), fn_name).entered()
        }

        /// Send our det thread id along with the actual message for both
        /// record and replay.
        pub fn send(&self, data: T) -> Result<(), ipc_channel::Error> {
            let _s = self.span(fn_basename!());

            match self.record_replay_send(data, &self.mode, &self.metadata, &self.recorder) {
                Ok(v) => v,
                Err((error, msg)) => {
                    match *DESYNC_MODE {
                        DesyncMode::Panic => {
                            panic!("Desynchronization detected: {}", error)
                        }
                        // TODO: One day we may want to record this alternate execution.
                        DesyncMode::KeepGoing => {
                            desync::mark_program_as_desynced();
                            rr::SendRecordReplay::underlying_send(self, get_det_id(), msg)
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

            let type_name = type_name::<T>().to_string();
            info!("Sender connected created: {:?} {:?}", id, type_name);

            let metadata = recordlog::RecordMetadata {
                type_name,
                channel_variant: ChannelVariant::Ipc,
                id,
            };
            ipc_channel::ipc::IpcSender::connect(name).map(|sender| IpcSender {
                sender,
                metadata,
                recorder: todo!(),
                mode: todo!(),
            })
        }
    }

    /// Encapsulate all logic for properly setting up channels. Called by channel() function.
    pub fn channel<T>() -> Result<(IpcSender<T>, IpcReceiver<T>), std::io::Error>
    where
        T: for<'de> Deserialize<'de> + Serialize,
    {
        let recorder = EventRecorder::get_global_recorder();
        let mode = get_rr_mode();

        let (sender, receiver) = ripc::channel()?;
        let id = DetChannelId::new();
        let type_name = type_name::<T>().to_string();

        let metadata = recordlog::RecordMetadata {
            type_name,
            channel_variant: ChannelVariant::Ipc,
            id: id.clone(),
        };

        let _s = span!(
            Level::INFO,
            "New Channel Created",
            dti = ?get_det_id(),
            variant = ?metadata.channel_variant,
            chan=%id
        )
        .entered();

        Ok((
            IpcSender::new(sender, metadata, recorder.clone(), mode),
            IpcReceiver::new(receiver, id, recorder, mode),
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
        index: u64,
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
            let recorder = EventRecorder::get_global_recorder();

            ipc_channel::ipc::IpcReceiverSet::new().map(|r| IpcReceiverSet {
                receiver_set: r,
                mode: get_rr_mode(),
                index: 0,
                receivers: Vec::new(),
                buffer: HashMap::new(),
                desynced: false,
                dummy_senders: Vec::new(),
                recorder,
            })
        }

        fn span(&self, fn_name: &str) -> EnteredSpan {
            span!(Level::INFO, stringify!(IpcReceiverSet), fn_name).entered()
        }

        pub fn add<T>(&mut self, receiver: IpcReceiver<T>) -> Result<u64, std::io::Error>
        where
            T: for<'de> Deserialize<'de> + Serialize,
        {
            let _e = self.span(fn_basename!());
            self.do_add(receiver.to_opaque())
        }

        pub fn add_opaque(&mut self, receiver: OpaqueIpcReceiver) -> Result<u64, std::io::Error> {
            let _e = self.span(fn_basename!());
            self.do_add(receiver)
        }

        fn do_add(&mut self, receiver: OpaqueIpcReceiver) -> Result<u64, std::io::Error> {
            self.rr_add(receiver)
                .unwrap_or_else(|(e, receiver)| match *DESYNC_MODE {
                    DesyncMode::Panic => {
                        panic!("Desynchronization detected: {}", e);
                    }
                    DesyncMode::KeepGoing => {
                        desync::mark_program_as_desynced();
                        self.move_receivers_to_set();
                        self.receiver_set.add_opaque(receiver.opaque_receiver)
                    }
                })
        }

        pub fn select(&mut self) -> Result<Vec<IpcSelectionResult>, std::io::Error> {
            let _s = self.span(fn_basename!());
            info!("Selecting");

            self.rr_select().unwrap_or_else(|(e, _entries)| {
                match *DESYNC_MODE {
                    DesyncMode::Panic => {
                        panic!("Desynchronization detected: {}", e);
                    }
                    DesyncMode::KeepGoing => {
                        desync::mark_program_as_desynced();
                        trace!("Looking for entries in buffer or call to select()...");

                        self.move_receivers_to_set();
                        // First use up our buffered entries, if none. Do the
                        // actual wait.
                        match self.get_buffered_entries() {
                            None => {
                                debug!("Doing direct do_select()");
                                self.do_select()
                            }
                            Some(be) => {
                                debug!("Using buffered entries.");
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
                let error = DesyncError::Desynchronized;
                error!(%error);
                return Err((error, vec![]));
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
                                recorded_events
                                    .push(IpcSelectEvent::MessageReceived(index, det_id));
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
                    self.recorder
                        .write_event_to_record(
                            event,
                            &recordlog::RecordMetadata::new(
                                "IpcSelect".to_string(),
                                ChannelVariant::IpcSelect,
                                DetChannelId::fake(),
                            ),
                        )
                        .expect("TODO");
                    Ok(Ok(moved_events))
                }

                // Use index to fetch correct receiver and read value directly from
                // receiver.
                RRMode::Replay => {
                    if self.desynced {
                        // TODO: Why are we returning empty vecs in this?
                        let error = DesyncError::Desynchronized;
                        error!(%error);
                        return Err((error, vec![]));
                    }

                    // Here we put this thread to sleep if the entry is missing.
                    // On record, the thread never returned from blocking...
                    let recorded_event = self.recorder.get_log_entry();

                    if let Err(e @ DesyncError::NoEntryInLog) = recorded_event {
                        info!("Saw {:?}. Putting thread to sleep.", e);
                        desync::sleep_until_desync();
                        // Thread woke back up... desynced!

                        let e1 = DesyncError::DesynchronizedWakeup;
                        error!(%e1);
                        return Err((e1, vec![]));
                    }

                    match recorded_event.map_err(|e| (e, vec![]))?.event {
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

                            Ok(Ok(events))
                        }
                        event => {
                            let dummy = RecordedEvent::IpcSelect {
                                select_events: vec![],
                            };
                            let error = DesyncError::EventMismatch(event, dummy);
                            error!(%error);
                            Err((error, vec![]))
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
            let _s = self.span(fn_basename!());
            trace!("replaying selected event");

            match event {
                IpcSelectEvent::MessageReceived(index, expected_sender) => {
                    let receiver = self
                        .receivers
                        .get_mut(*index as usize)
                        .ok_or_else(|| (DesyncError::MissingReceiver(*index), None))?;

                    // Auto buffers results.
                    Ok(IpcReceiverSet::do_replay_recv_for_entry(
                        &mut self.buffer,
                        *index,
                        expected_sender,
                        receiver,
                    )?)
                }
                IpcSelectEvent::ChannelClosed(index) => {
                    let receiver = self
                        .receivers
                        .get_mut(*index as usize)
                        .ok_or_else(|| (DesyncError::MissingReceiver(*index), None))?;

                    match IpcReceiverSet::rr_recv(receiver) {
                        Err(RecvErrorRR::Disconnected) => {
                            trace!("replay_select_event(): Saw channel closed for {:?}", index);
                            // remember that this receiver closed!
                            receiver.closed = true;
                            Ok(IpcSelectionResult::ChannelClosed(*index))
                        }
                        Err(RecvErrorRR::Timeout) => {
                            let e = DesyncError::Timeout;
                            error!(%e);
                            Err((e, None))
                        }
                        Ok(val) => {
                            // Expected a channel closed, saw a real message...
                            let error = DesyncError::ChannelClosedExpected;
                            error!(%error);
                            Err((
                                error,
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
            // TODO This doesn't look very good... 10 loops to "create" a second
            // timeout.
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
            let _s = span!(Level::INFO, stringify!(IpcReceiverSet), ?expected_sender).entered();

            if let Some(entry) = buffer
                .get_mut(&index)
                .and_then(|m| m.get_mut(expected_sender))
                .and_then(|e| e.pop_front())
            {
                trace!("Recv message found in buffer.");
                return Ok(IpcSelectionResult::MessageReceived(index, entry));
            }

            trace!("Looking for correct message in channel.");
            loop {
                let (data, channels, shared_memory_regions) =
                    // TODO timeout here!
                    match IpcReceiverSet::rr_recv(receiver) {
                        Ok(v) => v,
                        Err(RecvErrorRR::Disconnected) => {
                            trace!("Expected message, saw channel closed.");
                            // remember that this receiver closed!
                            receiver.closed = true;

                            let error = DesyncError::ChannelClosedUnexpected(index);
                            error!(%error);
                            return Err((error,
                                        Some(IpcSelectionResult::ChannelClosed(index))));
                        }
                        Err(RecvErrorRR::Timeout) => {
                            let error = DesyncError::Timeout;
                            error!(%error);
                            return Err((error, None));
                        }
                    };

                let msg = OpaqueIpcMessage::new(data, channels, shared_memory_regions);
                let det_id = IpcReceiverSet::deserialize_id(&msg);

                if &det_id == expected_sender {
                    trace!("message found via recv()");
                    return Ok(IpcSelectionResult::MessageReceived(index, msg));
                } else {
                    trace!(
                        "Wrong message received from {:?}, adding it to buffer",
                        det_id
                    );
                    buffer
                        .entry(index)
                        .or_insert_with(HashMap::new)
                        .entry(det_id)
                        .or_insert_with(VecDeque::new)
                        .push_back(msg);
                }
            }
        }

        /// Call actual select for real IpcReceiverSet and convert their
        /// ipc_channel::ipc::IpcSelectionResult into our IpcSelectionResult.
        fn do_select(&mut self) -> Result<Vec<IpcSelectionResult>, std::io::Error> {
            let _s = self.span(fn_basename!());

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
            info!("Found {:?} entries.", selected.len());
            Ok(selected)
        }

        /// On error we return the back to the user, otherwise it would be moved forever.
        fn rr_add(
            &mut self,
            receiver: OpaqueIpcReceiver,
        ) -> Result<Result<u64, io::Error>, (DesyncError, OpaqueIpcReceiver)> {
            if desync::program_desyned() {
                let error = DesyncError::Desynchronized;
                error!(%error);
                return Err((error, receiver));
            }

            // After the first time we desynchronize, this is set to true and future times
            // we will be sent here.
            if self.desynced {
                let error = DesyncError::Desynchronized;
                error!(%error);
                return Err((error, receiver));
            }

            match self.mode {
                RRMode::Record => {
                    let index = match self.receiver_set.add_opaque(receiver.opaque_receiver) {
                        Ok(v) => v,
                        e @ Err(_) => return Ok(e),
                    };

                    let event = RecordedEvent::IpcSelectAdd(index);
                    self.recorder
                        .write_event_to_record(event, &receiver.metadata)
                        // We use expect instead of '?' because I wouldn't know what (OpaqueIpcReceiver
                        // to return up from here? It has been consumed.
                        .expect("Unable to write entry to log.");

                    Ok(Ok(index))
                }

                // Do not add receiver to IpcReceiverSet, instead move the receiver to our own
                // `receivers` hashmap where the index returned here is the key.
                RRMode::Replay => {
                    let record_entry = match self.recorder.get_log_entry() {
                        Err(e) => {
                            return Err((e, receiver));
                        }
                        Ok(v) => v,
                    };

                    let metadata = &receiver.metadata;
                    let index = self.index;

                    if let Err(e) = self.rr_add_check_mismatch(record_entry, metadata) {
                        return Err((e, receiver));
                    }

                    self.receivers.insert(self.index as usize, receiver);

                    self.index += 1;
                    Ok(Ok(index))
                }
                RRMode::NoRR => Ok(self.receiver_set.add_opaque(receiver.opaque_receiver)),
            }
        }

        /// Performs error checking for rr_add function.
        fn rr_add_check_mismatch(
            &self,
            recorded_entry: RecordEntry,
            metadata: &RecordMetadata,
        ) -> Result<(), DesyncError> {
            match recorded_entry.event {
                RecordedEvent::IpcSelectAdd(expected_index) => {
                    if expected_index != self.index {
                        let error = DesyncError::SelectIndexMismatch(expected_index, self.index);
                        error!(%error);
                        return Err(error);
                    }

                    if self.receivers.get(self.index as usize).is_some() {
                        let error = DesyncError::SelectExistingEntry(self.index);
                        error!(%error);
                        return Err(error);
                    }

                    recorded_entry.check_mismatch(metadata)?;

                    Ok(())
                }
                event => {
                    let error =
                        DesyncError::EventMismatch(event, RecordedEvent::IpcSelectAdd(self.index));
                    error!(%error);
                    Err(error)
                }
            }
        }

        /// Fetch all buffered entries and treat them as the results of a
        /// select.
        fn get_buffered_entries(&mut self) -> Option<Vec<IpcSelectionResult>> {
            let _s = self.span(fn_basename!());
            trace!("Desynchronization detected. Fetching buffered Entries.");

            let mut entries = vec![];
            // Add all events that we have buffered up to this point if any.
            for (index, mut hashmap) in self.buffer.drain() {
                for (_, mut queue) in hashmap.drain() {
                    for entry in queue.drain(..) {
                        trace!("Entry found through buffer!");
                        entries.push(IpcSelectionResult::MessageReceived(index, entry));
                    }
                }
            }
            if entries.is_empty() {
                trace!("No buffered entries.");
                None
            } else {
                for e in &entries {
                    match e {
                        IpcSelectionResult::MessageReceived(i, _) => {
                            trace!("IpcSelectionResult::MessageReceived({:?}, _)", i);
                        }
                        IpcSelectionResult::ChannelClosed(i) => {
                            trace!("IpcSelectionResult::ChannelClosed({:?}, _)", i);
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
            let _s = self.span(fn_basename!());

            // Move all our receivers into the receiver set.
            if !self.desynced {
                debug!("Moving all receivers into actual select set.");

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

                        trace!("Adding dummy receiver at index {:?}", index);
                    } else {
                        // Add the real receiver.
                        let id = r.metadata.id.clone();
                        let i = self
                            .receiver_set
                            .add_opaque(r.opaque_receiver)
                            .expect("Unable to add receiver while handling desynchronization.");

                        trace!("Added receiver {:?} with index: {:?}", id, i);
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
    use crate::ipc_channel::ipc;
    use crate::recordlog::take_global_memory_recorder;
    use crate::recordlog::{ChannelVariant, RecordedEvent};
    use crate::test;
    use crate::test::set_rr_mode;
    use crate::test::{rr_test, Receiver, Sender, TestChannel, ThreadSafe, TryReceiver};
    use crate::RRMode;
    use crate::Tivo;
    use anyhow::Result;
    use ipc_channel::ipc::{IpcError, TryRecvError};
    use rusty_fork::rusty_fork_test;
    use serde::export::Formatter;
    use serde::{Deserialize, Serialize};
    use std::error::Error;

    #[derive(Debug, Eq, PartialEq)]
    pub(crate) enum MyIpcError {
        Empty,
        Disconnected,
        Io,
        BinCode,
        SendError,
    }

    impl std::fmt::Display for MyIpcError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self)
        }
    }

    impl Error for MyIpcError {}

    impl From<ipc::IpcError> for MyIpcError {
        fn from(e: IpcError) -> Self {
            match e {
                IpcError::Bincode(_) => MyIpcError::BinCode,
                IpcError::Io(_) => MyIpcError::Io,
                IpcError::Disconnected => MyIpcError::Disconnected,
            }
        }
    }

    impl From<Box<ipc_channel::ErrorKind>> for MyIpcError {
        fn from(_e: Box<ipc_channel::ErrorKind>) -> Self {
            // Drop error for now :grimace-emoji;
            MyIpcError::SendError
        }
    }

    impl<T: ThreadSafe + Serialize> Sender<T> for ipc::IpcSender<T> {
        type SendError = MyIpcError;

        fn send(&self, msg: T) -> Result<(), Self::SendError> {
            ipc::IpcSender::send(self, msg)?;
            Ok(())
        }
    }

    impl<T: for<'de> Deserialize<'de> + Serialize> Receiver<T> for ipc::IpcReceiver<T> {
        type RecvError = MyIpcError;

        fn recv(&self) -> Result<T, MyIpcError> {
            let v = ipc::IpcReceiver::recv(self)?;
            Ok(v)
        }
    }

    impl From<ipc::TryRecvError> for MyIpcError {
        fn from(e: TryRecvError) -> Self {
            match e {
                TryRecvError::IpcError(e) => e.into(),
                TryRecvError::Empty => MyIpcError::Empty,
            }
        }
    }

    impl<T: for<'de> Deserialize<'de> + Serialize> TryReceiver<T> for ipc::IpcReceiver<T> {
        type TryRecvError = MyIpcError;

        fn try_recv(&self) -> Result<T, Self::TryRecvError> {
            match ipc::IpcReceiver::try_recv(self) {
                Ok(v) => Ok(v),
                Err(e) => Err(e.into()),
            }
        }

        const EMPTY: Self::TryRecvError = MyIpcError::Empty;
        const TRY_DISCONNECTED: Self::TryRecvError = MyIpcError::Disconnected;
    }

    enum Ipc {}
    impl<T> TestChannel<T> for Ipc
    where
        T: ThreadSafe + Serialize + for<'de> Deserialize<'de>,
    {
        type S = ipc::IpcSender<T>;
        type R = ipc::IpcReceiver<T>;

        fn make_channels() -> (Self::S, Self::R) {
            ipc::channel().expect("Cannot init channels")
        }
    }

    rusty_fork_test! {
    #[test]
    fn ipc_channel_test() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);
        let (_s, _r) = ipc::channel::<i32>()?;
        Ok(())
    }

    #[test]
    fn simple_program_record_test() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::Record);
        test::simple_program::<Ipc>()?;

        let reference = test::simple_program_manual_log(
            RecordedEvent::IpcSender,
            |dti| RecordedEvent::IpcRecvSucc { sender_thread: dti },
            ChannelVariant::Ipc,
        );
        assert_eq!(reference, take_global_memory_recorder());
        Ok(())
    }

    #[test]
    fn recv_program_passthrough_test() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);
        test::recv_program::<Ipc>()
    }

    #[test]
    fn ipc_test_try_recv_passthrough() -> Result<()> {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);
        test::try_recv_program::<Ipc>()
    }

    #[test]
    fn recv_program_test() -> Result<()> {
        rr_test(test::recv_program::<Ipc>)
    }

    #[test]
    fn try_recv_program() -> Result<()> {
        rr_test(test::try_recv_program::<Ipc>)
    }

    // Ipc does not implement timeout functionality.
    }
}
