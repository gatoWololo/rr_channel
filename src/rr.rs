//! Different Channel types share the same general method of doing RR through our
//! per-channel buffers. This module encapsulates that logic via the
use crate::DetThreadId;
use crate::RRMode;
use crate::RECORD_MODE;
use lazy_static::lazy_static;
use std::fs::remove_file;
use std::fs::File;
use std::sync::{Mutex, Condvar, Arc};
use std::thread;

use crate::{DESYNC_MODE, DesyncMode, LOG_FILE_NAME};
use crate::log_rr;
use log::Level::*;
use crate::thread::get_det_id;
use crate::thread::get_event_id;
use crate::thread::get_temp_det_id;
use crate::thread::{inc_event_id , get_and_inc_channel_id, in_forwarding};
use crossbeam_channel::{RecvError, RecvTimeoutError, TryRecvError};
use std::sync::mpsc;
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

impl From<RecvErrorRR> for DesyncError {
    fn from(e: RecvErrorRR) -> DesyncError {
        match e {
            RecvErrorRR::Disconnected => DesyncError::Disconnected,
            RecvErrorRR::Timeout => DesyncError::Timedout,
        }
    }
}

/// Desynchonizations are common (TODO: make desync less common). This error type
/// enumerates all reasons why we might desynchronize when executing RR.
#[derive(Debug)]
pub enum DesyncError {
    /// Missing entry for specified (DetThreadId, EventIt) pair.
    NoEntryInLog(DetThreadId, EventId),
    /// ChannelVariants don't match. log flavor, vs current flavor.
    ChannelVariantMismatch(ChannelLabel, ChannelLabel),
    /// Channel ids don't match. log channel, vs current flavor.
    ChannelMismatch(DetChannelId, DetChannelId),
    /// RecordedEvent log message was different. logged event vs current event.
    EventMismatch(RecordedEvent, RecordedEvent),
    UnitializedDetThreadId,
    /// Receiver was expected for select operation but entry is missing.
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
    /// Receiver disconnected...
    Disconnected,
    /// Expected a message, but channel returned closed.
    ChannelClosedUnexpected(u64),
    /// Waited too long and no message ever came.
    Timedout,
    /// Thread woke up after assuming it had ran of the end of the log.
    /// While `NoEntryInLog` may mean that this thread simply blocked forever
    /// or "didn't make it this far", DesynchronizedWakeup means we put that
    /// thread to sleep and it was woken up by a desync somewhere.
    DesynchronizedWakeup,
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

/// We tag our record-log entries with the type of channel they represent. This lets us compares
/// their channel-type to ensure it is what we expect between record and replay. Just one more
/// sanity check for robustness.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Copy, Clone)]
pub enum ChannelLabel {
    MpscBounded,
    MpscUnbounded,
    Unbounded,
    After,
    Bounded,
    Never,
    None,
    Ipc,
    IpcSelect,
    Select,
}

/// Represents a single entry in our log. Technically all that is needed is the current_thread_id,
/// event_id, and event. Everything else is metadata used for checks to ensure we're reading from
/// the right channel.
#[derive(Serialize, Deserialize, Debug)]
pub struct LogEntry {
    /// Thread performing the operation's unique ID.
    /// (current_thread, event_id) form a unique key per entry in our map and log.
    pub current_thread: Option<DetThreadId>,
    /// Unique per-thread identifier given to every select dynamic instance.
    /// (current_thread, event_id) form a unique key per entry in our map and log.
    pub event_id: u32,
    /// Actual event and all information needed to replay this event.
    pub event: RecordedEvent,
    pub channel: ChannelLabel,
    pub chan_id: DetChannelId,
    // pub real_thread_id: String,
    // pub pid: u32,
    pub type_name: String,
}

/// Not sure why we use a dummy error here. TODO.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcDummyError;

/// Main enum listing different types of events that our logger supports. These recorded
/// events contain all information to replay the event. On errors, we can just return
/// the error that was witnessed during record. On successful read, the variant contains
/// the index or sender_thread_id where we should expect the message to arrive from.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RecordedEvent {
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

    // MPSC recv.
    MpscRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "MpscRecvErrorDef")]
    MpscRecvErr(mpsc::RecvError),

    // Crossbeam try_recv
    TryRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "TryRecvErrorDef")]
    TryRecvErr(TryRecvError),

    // MPSC try_recv
    MpscTryRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "MpscTryRecvErrorDef")]
    MpscTryRecvErr(mpsc::TryRecvError),

    // Crossbeam recv_timeout
    RecvTimeoutSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "RecvTimeoutErrorDef")]
    RecvTimeoutErr(RecvTimeoutError),

    // MPSC recv_timeout
    MpscRecvTimeoutSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "MpscRecvTimeoutErrorDef")]
    MpscRecvTimeoutErr(mpsc::RecvTimeoutError),

    // IPC recv
    IpcRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    IpcRecvErr(IpcDummyError),

    // Crossbeam send.
    Sender,
    // MPSC (standard library) send
    MpscSender,
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
    if ! DESYNC.load(Ordering::SeqCst) {
        log_rr!(Warn, "Program marked as desynced!");
        DESYNC.store(true, Ordering::SeqCst);
        wake_up_threads();
    }
}

pub(crate) fn sleep_until_desync() {
    log_rr!(Debug, "Thread being put to sleep.");
    let (lock, condvar) = &*PARK_THREADS;
    let mut started = lock.lock().expect("Unable to lock mutex.");
    while !*started {
        started = condvar.wait(started).expect("Unable to wait on condvar.");
    }
    log_rr!(Debug, "Thread woke up from eternal slumber!");
}

pub(crate) fn wake_up_threads() {
    log_rr!(Debug, "Waking up threads!");
    let (lock, condvar) = &*PARK_THREADS;
    let mut started = lock.lock().expect("Unable to lock mutex.");
    *started = true;
    condvar.notify_all();
}

lazy_static! {
    /// When we run off the end of the log,
    /// if we're not DESYNC, we assume that thread blocked forever during record
    /// if any other thread ever DESYNCs we let the thread continue running
    /// to avoid deadlocks. This condvar implements that logic.
    pub static ref PARK_THREADS: (Mutex<bool>, Condvar) = {
        (Mutex::new(false), Condvar::new())
    };

    /// Global bool keeping track if any thread has desynced.
    /// TODO: This may be too heavy handed. If we remove this bool, we get
    /// per event desync checks, which may be better than a global concept?
    pub static ref DESYNC: AtomicBool = {
        AtomicBool::new(false)
    };

    /// Global log file which all threads write to.
    pub static ref WRITE_LOG_FILE: Mutex<File> = {
        log_rr!(Debug, "Initializing WRITE_LOG_FILE lazy static.");

        match *RECORD_MODE {
            RRMode::Record => {
                // Delete file if it already existed... file may not exist. That's okay.
                let _ = remove_file(LOG_FILE_NAME.as_str());
                let error = & format!("Unable to open {} for record logging.", *LOG_FILE_NAME);
                Mutex::new(File::create(LOG_FILE_NAME.as_str()).expect(error))
            }
            RRMode::Replay =>
                panic!("Write log file should not be accessed in replay mode."),
            RRMode::NoRR =>
                panic!("Write log file should not be accessed in no record-and-replay mode."),
        }
    };

    /// Global map holding all indexes from the record phase.
    /// Lazily initialized on replay mode.
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (RecordedEvent, ChannelLabel, DetChannelId)> = {
        log_rr!(Debug, "Initializing RECORDED_INDICES lazy static.");
        use std::io::BufReader;

        let mut recorded_indices = HashMap::new();
        let log = File::open(LOG_FILE_NAME.as_str()).
            expect(& format!("Unable to open {} for replay.", LOG_FILE_NAME.as_str()));
        let log = BufReader::new(log);

        for line in log.lines() {
            let line = line.expect("Unable to read recorded log file");
            let entry: LogEntry = serde_json::from_str(&line).expect("Malformed log entry.");

            if entry.current_thread.is_none() {
                log_rr!(Trace, "Skipping None entry in log for entry: {:?}", entry);
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

pub fn log(event: RecordedEvent, channel: ChannelLabel, type_name: &str, chan_id: &DetChannelId) {
    log_rr!(Debug, "rr::log()");
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

    log_rr!(Info, "{:?} Logged entry: {:?}", entry, (get_det_id(), event_id));
    inc_event_id();
}

/// Actual implementation. Compares `curr_flavor` and `curr_id` against
/// log entry if they're Some(_).
fn get_log_entry_do<'a>(
    our_thread: DetThreadId,
    event_id: EventId,
    curr_flavor: Option<&ChannelLabel>,
    curr_id: Option<&DetChannelId>)
    -> Result<(&'a RecordedEvent, &'a ChannelLabel, &'a DetChannelId), DesyncError> {

    let key = (our_thread, event_id);
    match RECORDED_INDICES.get(&key) {
        Some((recorded, log_flavor, log_id)) => {
            log_rr!(Debug, "Event fetched: {:?} for keys: {:?}",
                    (recorded, log_flavor, log_id), key);

            if let Some(curr_flavor) = curr_flavor {
                if log_flavor != curr_flavor {
                    return Err(DesyncError::ChannelVariantMismatch(*log_flavor, *curr_flavor));
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
                              curr_flavor: &ChannelLabel,
                              curr_id: &DetChannelId)
                              -> Result<&'a RecordedEvent, DesyncError> {
    get_log_entry_do(our_thread, event_id, Some(curr_flavor), Some(curr_id)).
        map(|(r, _, _)| r)
}

/// Get log entry with no comparison.
pub fn get_log_entry<'a>(our_thread: DetThreadId,
                         event_id: EventId) -> Result<&'a RecordedEvent, DesyncError> {
    get_log_entry_do(our_thread, event_id, None, None).map(|(r, _, _)| r)
}

/// Get log entry returning the record, flavor, and channel id if needed.
pub fn get_log_entry_ret<'a>(our_thread: DetThreadId, event_id: EventId)
                             -> Result<(&'a RecordedEvent, &'a ChannelLabel, &'a DetChannelId),
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

/// TODO It seems there is repetition in the fields here and on LogEntry?
/// Do we really need both?
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecordMetadata {
    /// Owned for EZ serialize, deserialize.
    pub(crate) type_name: String,
    pub(crate) flavor: ChannelLabel,
    pub(crate) mode: RRMode,
    /// Unique identifier assigned to every channel. Deterministic and unique
    /// even with racing thread creation. DetThreadId refers to the original
    /// creator of this thread.
    /// The partner Receiver and Sender shares the same id.
    pub(crate) id: DetChannelId,
}

use crate::rr;
use std::error::Error;

pub trait RecvRR<T, E: Error> {
    fn rr_recv(
        &self,
        metadata: &RecordMetadata,
        recv: impl FnOnce() -> Result<(Option<DetThreadId>, T), E>,
        function_name: &str,
    ) -> Result<Result<T, E>, DesyncError> {
        log_rr!(Debug, "Receiver<{:?}>::{}", metadata.id, function_name);

        match metadata.mode {
            RRMode::Record => {;
                               let (result, recorded) = self.to_recorded_event(recv());
                               rr::log(recorded, metadata.flavor,
                                       &metadata.type_name, &metadata.id);
                               Ok(result)
            }
            RRMode::Replay => {
                match get_det_id() {
                    None => {
                        log_rr!(Warn, "DetThreadId was None. \
                                       This execution may not be deterministic.");
                        inc_event_id();
                        // TODO: This seems wrong. I think we should be doing
                        // a replay_recv() here even if the det_id is none.
                        Ok(recv().map(|v| v.1))
                    }
                    Some(det_id) => {
                        let entry = get_log_entry_with(det_id,
                                                       get_event_id(),
                                                       &metadata.flavor,
                                                       &metadata.id);

                        // Special case for NoEntryInLog. Hang this thread forever.
                        if let Err(e@DesyncError::NoEntryInLog(_, _)) = entry {
                            log_rr!(Info, "Saw {:?}. Putting thread to sleep.", e);
                            sleep_until_desync();
                            // Thread woke back up... desynced!
                            return Err(DesyncError::DesynchronizedWakeup);
                        }

                        Ok(Self::expected_recorded_events(self, entry?.clone())?)
                    }
                }
            }
            RRMode::NoRR => Ok(recv().map(|v| v.1)),
        }
    }

    /// Handles desynchronization events based on global DESYNC_MODE.
    fn handle_desync(&self,
                     desync: DesyncError,
                     can_block: bool,
                     do_recv: impl Fn() -> Result<(Option<DetThreadId>, T), E>)
                     -> Result<T, E> {
        log_rr!(Warn, "Desynchonization found: {:?}", desync);

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
                         -> (Result<T, E>, RecordedEvent);

    fn expected_recorded_events(&self, event: RecordedEvent) -> Result<Result<T, E>, DesyncError>;
}

/// Abstract over logic to send a message while recording or replaying results.
pub(crate) trait SendRR<T, E> {
    /// RecordedEvent variant for this type.
    const EVENT_VARIANT: RecordedEvent;

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
        flavor: &ChannelLabel,
        sender_name: &str,
    ) -> Result<Result<(), E>, (DesyncError, T)> {

        if program_desyned() {
            return Err((DesyncError::Desynchronized, msg));
        }

        // However, for the record log, we still want to use the original
        // thread's DetThreadId. Otherwise we will have "repeated" entries in the log
        // which look like they're coming from the same thread.
        let forwading_id = get_forward_id();
        log_rr!(Info, "{}<{:?}>::send(({:?}, {:?}))",
                sender_name, id, forwading_id, type_name);

        match mode {
            RRMode::Record => {
                // Note: send() must come before rr::log() as it internally
                // increments event_id.
                let result = self.send(forwading_id, msg);
                rr::log(Self::EVENT_VARIANT, flavor.clone(), type_name, id);
                Ok(result)
            }
            RRMode::Replay => {
                if let Some(det_id) = get_det_id() {
                    // Ugh. This is ugly. I need it though. As this function moves the `T`.
                    // If we encounter an error we need to return the `T` back up to the caller.
                    // crossbeam_channel::send() does pretty much the same thing.
                    match rr::get_log_entry_with(det_id, get_event_id(), flavor, id) {
                        // Special case for NoEntryInLog. Hang this thread forever.
                        Err(e@DesyncError::NoEntryInLog(_, _)) => {
                            log_rr!(Info, "Saw {:?}. Putting thread to sleep.", e);
                            sleep_until_desync();
                            // Thread woke back up... desynced!
                            return Err((DesyncError::DesynchronizedWakeup, msg));
                        }
                        // TODO: Hmmm when does this error case happen?
                        Err(e) => return Err((e, msg)),
                        Ok(event) => {
                            if let Err(e) = SendRR::check_log_entry(self, event.clone()) {
                                return Err((e, msg));
                            }
                        }
                    }
                } else {
                    log_rr!(Warn, "det_id is None. This execution may be nondeterministic");
                }

                let result = SendRR::send(self, forwading_id, msg);
                inc_event_id();
                Ok(result)
            }
            RRMode::NoRR => {
                Ok(self.send(get_det_id(), msg))
            }
        }
    }

    /// Given the current entry in the log checks that the expected `RecordedEvent` variant is
    /// present.
    fn check_log_entry(&self, entry: RecordedEvent) -> Result<(), DesyncError>;

    /// Call underlying channel's send function to send this message and thread_id to receiver.
    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), E>;
}

/// Generic function to loop on differnt kinds of replays: recv(), try_recv(), etc.
/// and buffer wrong entries.
pub(crate) fn recv_from_sender<T>(
    expected_sender: &Option<DetThreadId>,
    rr_timeout_recv: impl Fn() -> Result<(Option<DetThreadId>, T), RecvErrorRR>,
    buffer: &mut RefMut<HashMap<Option<DetThreadId>, VecDeque<T>>>,
    id: &DetChannelId,
) -> Result<T, DesyncError> {

    log_rr!(Debug, "recv_from_sender(sender: {:?}, id: {:?}) ...", expected_sender, id);

    // Check our buffer to see if this value is already here.
    if let Some(val) = buffer.get_mut(expected_sender).and_then(|e| e.pop_front()) {
        log_rr!(Debug, "replay_recv(): Recv message found in buffer. Channel id: {:?}", id);
        return Ok(val);
    }

    // Loop until we get the message we're waiting for. All "wrong" messages are
    // buffered into self.buffer.
    loop {
        let (msg_sender, msg) = rr_timeout_recv()?;
        if msg_sender == *expected_sender {
            log_rr!(Debug, "Recv message found through recv()");
            return Ok(msg);
        } else {
            // Value did no match. Buffer it. Handles both `none_buffer` and
            // regular `buffer` case.
            log_rr!(Debug, "Wrong value found. Queing it for: {:?}", id);
            buffer.entry(msg_sender).or_insert(VecDeque::new()).push_back(msg);
        }
    }
}

/// Because of the router, we want to "forward" the original sender's DetThreadId
/// sometimes.
pub fn get_forward_id() -> Option<DetThreadId> {
    if in_forwarding() { get_temp_det_id() } else { get_det_id() }
}


#[test]
fn condvar_test() {
    let mut handles = vec![];
    for i in 1..10 {
        let h = thread::spawn(move || {
            println!("Putting thread {} to sleep.", i);
            sleep_until_desync();
            println!("Thread {} woke up!", i);
        });
        handles.push(h);
    }

    thread::sleep_ms(1000);
    println!("Main thread waking everyone up...");
    wake_up_threads();
    for h in handles {
        h.join().unwrap();
    }
    assert!(true);
}

// We want to Serialize Types defined in other crates. So we copy their definition here.
// (This is how serde wants us to do it)
#[derive(Serialize, Deserialize)]
#[serde(remote = "RecvTimeoutError")]
pub enum RecvTimeoutErrorDef {
    Timeout,
    Disconnected,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "mpsc::RecvTimeoutError")]
pub enum MpscRecvTimeoutErrorDef {
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
#[serde(remote = "mpsc::TryRecvError")]
pub enum MpscTryRecvErrorDef {
    Empty,
    Disconnected,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "RecvError")]
pub struct RecvErrorDef {}

#[derive(Serialize, Deserialize)]
#[serde(remote = "mpsc::RecvError")]
pub struct MpscRecvErrorDef {}
