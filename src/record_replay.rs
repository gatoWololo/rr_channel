use crate::DetThreadId;
use crate::RecordReplayMode;
use crate::RECORD_MODE;
use lazy_static::lazy_static;
use std::fs::remove_file;
use std::fs::File;
use std::sync::Mutex;
use std::thread;

use crate::{DESYNC_MODE, DesyncMode, LOG_FILE_NAME};
use crate::{log_trace};
use crate::log_trace_with;
use crate::thread::get_det_id;
use crate::thread::get_event_id;
use crate::thread::get_temp_det_id;
use crate::thread::{inc_event_id , get_and_inc_channel_id, in_forwarding};
use crossbeam_channel::{RecvError, RecvTimeoutError, TryRecvError};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;

use std::cell::RefMut;
use std::collections::VecDeque;

use std::sync::atomic::AtomicBool;

#[derive(Debug)]
pub enum RecvErrorRR {
    Timeout,
    Disconnected,
}

#[derive(Debug)]
pub enum DesyncError {
    /// Missing entry for specified (DetThreadId, EventIt) pair.
    NoEntryInLog(DetThreadId, EventId),
    /// Flavors don't match. log flavor, vs current flavor.
    FlavorMismatch(FlavorMarker, FlavorMarker),
    /// Channel ids don't match. log channel, vs current flavor.
    ChannelMismatch(DetChannelId, DetChannelId),
    /// Recorded log message was different. logged event vs current event.
    EventMismatch(Recorded, Recorded),
    UnitializedDetThreadId,
    MissingReceiver(u64),
    /// An entry already exists for the specificied index in IpcReceiverSet.
    SelectExistingEntry(u64),
    /// IpcReceiverSet expected first index, but found second index.
    SelectIndexMismatch(u64, u64),
    /// Generic error used for situations where we already know we're
    /// desynchronized and want to alert caller.
    Desynchronized,
    /// Expected a channel close message, but saw actual value.
    ChannelClosedExpected,
    /// Expected a message, but channel returned closed.
    ChannelClosedUnexpected(u64),
    /// Waited too long and no message ever came.
    Timedout,
}

/// Record representing a sucessful select from a channel. Used in replay mode.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SelectEvent {
    Success {
        /// For multiple producer channels we need to diffentiate who the sender was.
        /// As the index only tells us the correct receiver end, but not who the sender was.
        sender_thread: Option<DetThreadId>,
        /// Index of selected channel for select!()
        selected_index: usize,
    },
    RecvError {
        /// Index of selected channel for select!(). We still need this even onf RecvError
        /// as user might call .index() method on SelectedOperation and we need to be able
        /// to return the real index.
        selected_index: usize,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Copy, Clone)]
pub enum FlavorMarker {
    Unbounded,
    After,
    Bounded,
    Never,
    None,
    Ipc,
    IpcSelect,
    Select,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LogEntry {
    /// Thread performing the operation's unique ID.
    /// (current_thread, event_id) form a unique key per entry in our map and log.
    pub current_thread: Option<DetThreadId>,
    /// Unique per-thread identifier given to every select dynamic instance.
    /// (current_thread, event_id) form a unique key per entry in our map and log.
    pub event_id: u32,
    pub event: Recorded,
    pub channel: FlavorMarker,
    pub chan_id: DetChannelId,
    // pub real_thread_id: String,
    // pub pid: u32,
    pub type_name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "RecvTimeoutError")]
pub enum RecvTimeoutErrorDef {
    Timeout,
    Disconnected,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "TryRecvError")]
pub enum TryRecvErrorDef {
    Empty,
    Disconnected,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "RecvError")]
pub struct RecvErrorDef {}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcDummyError;

// Types of operating that may block.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum BlockingOp {
    Select,
    IpcSelect,
    Recv,
    IpcRecv,
}

/// Main enum listing different types of events
/// that our logger supports.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Recorded {
    /// Blocking operations may never return.
    /// We use this event to tell when a blocking operation
    /// was called but never returned.
    BlockingOpStart(BlockingOp),
    // Crossbeam select.
    SelectReady {
        select_index: usize,
    },
    Select(SelectEvent),

    // Crossbeam recv.
    RecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "RecvErrorDef")]
    RecvErr(RecvError),

    // Crossbeam try_recv
    TryRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "TryRecvErrorDef")]
    TryRecvErr(TryRecvError),

    // Crossbeam recv_timeout
    RecvTimeoutSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "RecvTimeoutErrorDef")]
    RecvTimeoutErr(RecvTimeoutError),

    // IPC recv
    IpcRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    IpcRecvErr(IpcDummyError),

    // Crossbeam send.
    Sender,
    // Ipc send.
    IpcSender,

    // IpcReceiverSet
    IpcSelectAdd(/*index:*/ u64),
    IpcSelect {
        select_events: Vec<IpcSelectEvent>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum IpcSelectEvent {
    MessageReceived(/*index:*/ u64, Option<DetThreadId>),
    ChannelClosed(/*index:*/ u64),
}

use std::sync::atomic::Ordering;
pub(crate) fn program_desyned() -> bool {
    DESYNC.load(Ordering::SeqCst)
}

pub(crate) fn mark_program_as_desynced() {
    DESYNC.store(true, Ordering::SeqCst)
}

lazy_static! {
    pub static ref DESYNC: AtomicBool = {
        AtomicBool::new(false)
    };

    /// Global log file which all threads write to.
    pub static ref WRITE_LOG_FILE: Mutex<File> = {
        log_trace("Initializing WRITE_LOG_FILE lazy static.");

        match *RECORD_MODE {
            RecordReplayMode::Record => {
                // Delete file if it already existed... file may not exist. That's okay.
                let _ = remove_file(LOG_FILE_NAME.as_str());
                let error = & format!("Unable to open {} for record logging.", *LOG_FILE_NAME);
                Mutex::new(File::create(LOG_FILE_NAME.as_str()).expect(error))
            }
            RecordReplayMode::Replay =>
                panic!("Write log file should not be accessed in replay mode."),
            RecordReplayMode::NoRR =>
                panic!("Write log file should not be accessed in no record-and-replay mode."),
        }
    };

    /// Global map holding all indexes from the record phase.
    /// Lazily initialized on replay mode.
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (Recorded, FlavorMarker, DetChannelId)> = {
        log_trace("Initializing RECORDED_INDICES lazy static.");
        use std::io::BufReader;

        let mut recorded_indices = HashMap::new();
        let log = File::open(LOG_FILE_NAME.as_str()).
            expect(& format!("Unable to open {} for replay.", LOG_FILE_NAME.as_str()));
        let log = BufReader::new(log);

        for line in log.lines() {
            let line = line.expect("Unable to read recorded log file");
            let entry: LogEntry = serde_json::from_str(&line).expect("Malformed log entry.");

            if entry.current_thread.is_none() {
                log_trace(&format!("Skipping None entry in log for entry: {:?}", entry));
                continue;
            }
            let curr_thread = entry.current_thread.clone().unwrap();
            let key = (curr_thread, entry.event_id);
            let value = (entry.event.clone(), entry.channel, entry.chan_id.clone());

            let prev = recorded_indices.insert(key, value);
            if prev.is_some() {
                panic!("Corrupted log file. Adding key-value ({:?}, {:?}) but previous value \
                        {:?} already exited. Hashmap entries should be unique.",
                       (entry.current_thread, entry.event_id),
                       (entry.event, entry.channel, entry.chan_id),
                       prev);
            }
        }

        recorded_indices
    };
}

pub fn log(event: Recorded, channel: FlavorMarker, type_name: &str, chan_id: &DetChannelId) {
    log_trace("record_replay::log()");
    use std::io::Write;

    // Write our (DET_ID, EVENT_ID) -> index to our log file
    let current_thread = get_det_id();
    let event_id = get_event_id();
    let tid = format!("{:?}", ::std::thread::current().id());
    let pid = std::process::id();
    let chan_id = chan_id.clone();

    let entry: LogEntry = LogEntry {
        current_thread,
        event_id,
        event,
        channel,
        chan_id,
        // real_thread_id: tid, pid,
        type_name: type_name.to_owned(),
    };
    let serialized = serde_json::to_string(&entry).unwrap();

    WRITE_LOG_FILE
        .lock()
        .expect("Unable to lock file.")
        .write_fmt(format_args!("{}\n", serialized))
        .expect("Unable to write to log file.");

    log_trace(&format!(
        "{:?} Logged entry: {:?}",
        entry,
        (get_det_id(), event_id)
    ));
    inc_event_id();
}

/// Actual implementation. Compares `curr_flavor` and `curr_id` against
/// log entry if they're Some(_).
fn get_log_entry_do<'a>(
    our_thread: DetThreadId,
    event_id: EventId,
    curr_flavor: Option<&FlavorMarker>,
    curr_id: Option<&DetChannelId>)
    -> Result<(&'a Recorded, &'a FlavorMarker, &'a DetChannelId), DesyncError> {

    let key = (our_thread, event_id);
    match RECORDED_INDICES.get(&key) {
        Some((recorded, log_flavor, log_id)) => {
            log_trace(&format!("Event fetched: {:?} for keys: {:?}",
                               (recorded, log_flavor, log_id), key));

            if let Some(curr_flavor) = curr_flavor {
                if log_flavor != curr_flavor {
                    return Err(DesyncError::FlavorMismatch(*log_flavor, *curr_flavor));
                }
            }
            if let Some(curr_id) = curr_id {
                if log_id != curr_id {
                    return Err(DesyncError::ChannelMismatch(log_id.clone(), curr_id.clone()));
                }
            }
            Ok((recorded, log_flavor, log_id))
        }
        None => Err(DesyncError::NoEntryInLog(key.0, key.1)),
    }
}

/// Get log entry and compare `curr_flavor` and `curr_id` with the values
/// on the record.
pub fn get_log_entry_with<'a>(our_thread: DetThreadId,
                              event_id: EventId,
                              curr_flavor: &FlavorMarker,
                              curr_id: &DetChannelId)
                              -> Result<&'a Recorded, DesyncError> {
    get_log_entry_do(our_thread, event_id, Some(curr_flavor), Some(curr_id)).
        map(|(r, _, _)| r)
}

/// Get log entry with no comparison.
pub fn get_log_entry<'a>(our_thread: DetThreadId,
                         event_id: EventId) -> Result<&'a Recorded, DesyncError> {
    get_log_entry_do(our_thread, event_id, None, None).map(|(r, _, _)| r)
}

/// Get log entry returning the record, flavor, and channel id if needed.
pub fn get_log_entry_ret<'a>(our_thread: DetThreadId, event_id: EventId)
                             -> Result<(&'a Recorded, &'a FlavorMarker, &'a DetChannelId),
                                       DesyncError> {
    get_log_entry_do(our_thread, event_id, None, None)
}

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
            det_thread_id: get_det_id(),
            channel_id: get_and_inc_channel_id(),
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

pub type EventId = u32;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecordMetadata {
    /// Unique identifier assigned to every channel. Deterministic and unique
    /// even with racing thread creation. DetThreadId refers to the original
    /// creator of this thread.
    /// The partner Receiver and Sender shares the same id.
    /// Owned for EZ serialize, deserialize.
    pub(crate) type_name: String,
    pub(crate) flavor: FlavorMarker,
    pub(crate) mode: RecordReplayMode,
    /// Unique identifier assigned to every channel. Deterministic and unique
    /// even with racing thread creation. DetThreadId refers to the original
    /// creator of this thread.
    /// The partner Receiver and Sender shares the same id.
    pub(crate) id: DetChannelId,
}

use crate::record_replay;
use std::error::Error;

#[derive(Debug, Eq, PartialEq)]
pub enum Blocking {
    CannotBlock,
    CanBlock,
}

pub trait RecordReplayRecv<T, E: Error> {
    fn record_replay_recv(
        &self,
        metadata: &RecordMetadata,
        recv: impl FnOnce() -> Result<(Option<DetThreadId>, T), E>,
        function_name: &str,
    ) -> Result<Result<T, E>, DesyncError> {
        log_trace(&format!("Receiver<{:?}>::{}", metadata.id, function_name));

        match metadata.mode {
            RecordReplayMode::Record => {
                // Log the fact that we started a possibly blocking operation!
                // TODO We don't need this for all variants e.g. try_recv.
                let record = Recorded::BlockingOpStart(BlockingOp::Recv);
                record_replay::log(record, metadata.flavor,
                                   &metadata.type_name, &metadata.id);
                let (result, recorded) = self.to_recorded_event(recv());
                record_replay::log(recorded, metadata.flavor,
                                   &metadata.type_name, &metadata.id);
                Ok(result)
            }
            RecordReplayMode::Replay => {
                match get_det_id() {
                    None => {
                        warn!("DetThreadId was None. \
                               This execution may not be deterministic.");
                        inc_event_id();
                        // TODO: This seems wrong. I think we should be doing
                        // a replay_recv() here even if the det_id is none.
                        Ok(recv().map(|v| v.1))
                    }
                    Some(det_id) => {
                        // We expect to see a start
                        let event = get_log_entry_with(det_id.clone(),
                                                       get_event_id(),
                                                       &metadata.flavor,
                                                       &metadata.id)?;
                        match event {
                            Recorded::BlockingOpStart(_ /*TODO*/) => { }
                            event => {
                                let dummy = Recorded::BlockingOpStart(BlockingOp::Recv);
                                let e = DesyncError::EventMismatch(dummy, event.clone());
                                return Err(e);
                            }
                        }
                        inc_event_id();
                        let entry = get_log_entry_with(det_id,
                                                       get_event_id(),
                                                       &metadata.flavor,
                                                       &metadata.id);

                        // Special case for NoEntryInLog. Hang this thread forever.
                        if let Err(e@DesyncError::NoEntryInLog(_, _)) = entry {
                            log_trace(&format!("Saw {:?}. Putting thread to sleep.", e));
                            loop { thread::park() }
                        }

                        Ok(self.expected_recorded_events(entry?.clone())?)
                    }
                }
            }
            RecordReplayMode::NoRR => Ok(recv().map(|v| v.1)),
        }
    }

    /// Handles desynchronization events based on global DESYNC_MODE.
    fn handle_desync(&self,
                        desync: DesyncError,
                        can_block: bool,
                        do_recv: impl Fn() -> Result<(Option<DetThreadId>, T), E>)
                     -> Result<T, E> {
        warn!("Desynchonization found: {:?}", desync);

        match *DESYNC_MODE {
            DesyncMode::KeepGoing => {
                mark_program_as_desynced();
                inc_event_id();
                self.desync_get_next_entry(&do_recv)
            }
            DesyncMode::Panic => {
                panic!("Desynchonization found: {:?}", desync);
            }
        }
    }

    fn desync_get_next_entry(&self,
                             do_recv: &impl Fn() -> Result<(Option<DetThreadId>, T), E>)
                             -> Result<T, E> {
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

    fn to_recorded_event(&self, event: Result<(Option<DetThreadId>, T), E>)
                         -> (Result<T, E>, Recorded);
    fn expected_recorded_events(&self, event: Recorded) -> Result<Result<T, E>, DesyncError>;
}


pub(crate) trait RecordReplaySend<T, E> {
    fn record_replay_send(
        &self,
        msg: T,
        mode: &RecordReplayMode,
        id: &DetChannelId,
        type_name: &str,
        flavor: &FlavorMarker,
        sender_name: &str,
    ) -> Result<Result<(), E>, (DesyncError, T)> {
        if program_desyned() {
            return Err((DesyncError::Desynchronized, msg));
        }
        // However, for the record log, we still want to use the original
        // thread's DetThreadId. Otherwise we will have "repeated" entries in the log
        // which look like they're coming from the same thread.
        let forwading_id = get_forward_id();
        log_trace(&format!(
            "{}<{:?}>::send(({:?}, {:?}))",
            sender_name, id, forwading_id, type_name
        ));

        match mode {
            RecordReplayMode::Record => {
                let event = self.as_recorded_event();
                // Note: send() must come before record_replay::log() as it internally
                // increments event_id.
                let result = self.send(forwading_id, msg);
                record_replay::log(event, flavor.clone(), type_name, id);
                Ok(result)
            }
            RecordReplayMode::Replay => {
                if let Some(det_id) = get_det_id() {
                    // Ugh. This is ugly. I need it though. As this function moves the `T`.
                    // If we encounter an error we need to return the `T` back up to the caller.
                    // crossbeam_channel::send() does pretty much the same thing.
                    match record_replay::get_log_entry_with(det_id, get_event_id(), flavor, id) {
                        Err(e) => return Err((e, msg)),
                        Ok(event) => {
                            match self.check_log_entry(event.clone()) {
                                Err(e) => return Err((e, msg)),
                                Ok(event) => {},
                            }
                        }
                    }
                } else {
                    warn!("det_id is None. This execution may be nondeterministic");
                }

                let result = self.send(forwading_id, msg);
                inc_event_id();
                Ok(result)
            }
            RecordReplayMode::NoRR => {
                Ok(self.send(get_det_id(), msg))
            }
        }
    }

    fn check_log_entry(&self, entry: Recorded) -> Result<(), DesyncError>;

    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), E>;

    fn as_recorded_event(&self) -> Recorded;
}

/// Because of the router, we want to "forward" the original sender's DetThreadId
/// sometimes.
pub fn get_forward_id() -> Option<DetThreadId> {
    if in_forwarding() { get_temp_det_id() } else { get_det_id() }
}

/// Generic function to loop on differnt kinds of replays: recv(), try_recv(), etc.
/// and buffer wrong entries.
pub fn recv_from_sender<T>(
    expected_sender: &Option<DetThreadId>,
    rr_try_recv: impl Fn() -> Result<(Option<DetThreadId>, T), RecvErrorRR>,
    buffer: &mut RefMut<HashMap<Option<DetThreadId>, VecDeque<T>>>,
    id: &DetChannelId,
) -> Result<T, DesyncError> {
    log_trace_with(
        &format!("recv_from_sender(expected_sender: {:?}) ...", expected_sender),
        id,
    );

    // Check our buffer to see if this value is already here.
    if let Some(val) = buffer.get_mut(expected_sender).and_then(|e| e.pop_front()) {
        log_trace_with("replay_recv(): Recv message found in buffer.", id);
        return Ok(val);
    }

    // Loop until we get the message we're waiting for. All "wrong" messages are
    // buffered into self.buffer.
    loop {
        match rr_try_recv() {
            // Value matches return it!
            Ok((msg_sender, msg)) if msg_sender == *expected_sender => {
                log_trace("Recv message found through recv()");
                return Ok(msg);
            }
            // Value did no match. Buffer it. Handles both `none_buffer` and
            // regular `buffer` case.
            Ok((msg_sender, msg)) => {
                let wrong_val = &format!("Wrong value found. Queing it for: {:?}", msg_sender);
                log_trace_with(wrong_val, id);
                buffer.entry(msg_sender).or_insert(VecDeque::new()).push_back(msg);
            }
            Err(RecvErrorRR::Disconnected) => {
                // Got a disconnected message. keep going...
                log_trace("Saw Discoonected, trying again.");
                continue
            }
            Err(RecvErrorRR::Timeout) => {
                return Err(DesyncError::Timedout);
            }
        }
    }
}
