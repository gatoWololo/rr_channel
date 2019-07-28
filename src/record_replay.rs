use crate::DetThreadId;
use crate::RecordReplayMode;
use crate::RECORD_MODE;
use lazy_static::lazy_static;
use std::fs::remove_file;
use std::fs::File;
use std::sync::Mutex;

use crate::log_trace;
use crate::log_trace_with;
use crate::thread::get_det_id;
use crate::thread::get_event_id;
use crate::thread::inc_event_id;
use crate::thread::in_forwarding;
use crossbeam_channel::{RecvError, RecvTimeoutError, TryRecvError};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;
use crate::thread::get_temp_det_id;

// TODO use environment variables to generalize this.
const LOG_FILE_NAME: &str = "/home/gatowololo/det_file.txt";

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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Recorded {
    SelectReady {
        select_index: usize,
    },
    Select(SelectEvent),

    RecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "RecvErrorDef")]
    RecvErr(RecvError),

    TryRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "TryRecvErrorDef")]
    TryRecvErr(TryRecvError),

    RecvTimeoutSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "RecvTimeoutErrorDef")]
    RecvTimeoutErr(RecvTimeoutError),

    IpcRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    // #[serde(with = "ErrorKindDef")]
    IpcRecvErr(IpcDummyError),

    Sender(DetChannelId),
    IpcSender(DetChannelId),
    IpcSelectAdd(/*index:*/ u64),
    IpcSelect {
        select_events: Vec<IpcSelectEvent>
    }
    // IpcRouterSelectSucc {
    //     select_indices: Vec<u64>,
    // }
    // IpcRouterSelectErr,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum IpcSelectEvent {
    MessageReceived(/*index:*/ u64, Option<DetThreadId>),
    ChannelClosed(/*index:*/ u64),
}

// pub enum IpcSelectionResult {
//     MessageReceived(index)
//         ChannelClosed(index)
// }

lazy_static! {
    /// Global log file which all threads write to.
    pub static ref WRITE_LOG_FILE: Mutex<File> = {
        log_trace("Initializing WRITE_LOG_FILE lazy static.");

        match *RECORD_MODE {
            RecordReplayMode::Record => {
                // Delete file if it already existed... file may not exist. That's okay.
                let _ = remove_file(LOG_FILE_NAME);
                let error = & format!("Unable to open {} for record logging.", LOG_FILE_NAME);
                Mutex::new(File::create(LOG_FILE_NAME).expect(error))
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
        let log = File::open(LOG_FILE_NAME).
            expect(& format!("Unable to open {} for replay.", LOG_FILE_NAME));
        let log = BufReader::new(log);

        for line in log.lines() {
            let line = line.expect("Unable to read recorded log file");
            let entry: LogEntry = serde_json::from_str(&line).expect("Malformed log entry.");

            if entry.current_thread.is_none() {
                log_trace(&format!("Skipping None entry in log for entry: {:?}", entry));
                continue;
            }

            let key = (entry.current_thread.clone().unwrap(), entry.event_id);
            let value = (entry.event.clone(), entry.channel, entry.chan_id.clone());

            let prev = recorded_indices.insert(key, value);
            if prev.is_some() {
                panic!("Failed to replay. Adding key-value ({:?}, {:?}) but previous value \
                        {:?} already exited. Hashmap entries should be unique.",
                       (entry.current_thread, entry.event_id),
                       (entry.event, entry.channel, entry.chan_id),
                       prev);
            }

        }

        // log_trace("{:?}", recorded_indices);
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
        .unwrap()
        .write_fmt(format_args!("{}\n", serialized))
        .expect("Unable to write to log file.");

    log_trace(&format!("{:?} Logged entry: {:?}", entry, (get_det_id(), event_id)));
    inc_event_id();
}

pub fn get_log_entry<'a>(
    our_thread: DetThreadId,
    event_id: EventId,
) -> Option<&'a (Recorded, FlavorMarker, DetChannelId)> {
    let key = (our_thread, event_id);
    let log_entry = RECORDED_INDICES.get(&key);

    log_trace(&format!(
        "Event fetched: {:?} for keys: {:?}",
        log_entry, key
    ));
    log_entry
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct DetChannelId {
    pub det_thread_id: Option<DetThreadId>,
    pub channel_id: u32
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

pub trait RecordReplay<T, E: Error> {
    fn record_replay(
        &self,
        metadata: &RecordMetadata,
        func: impl FnOnce() -> Result<(Option<DetThreadId>, T), E>,
        blocking: Blocking,
        function_name: &str,
    ) -> Result<T, E> {
        log_trace(&format!("Receiver<{:?}>::{}", metadata.id, function_name));

        match metadata.mode {
            RecordReplayMode::Record => {
                let (result, event) = self.to_recorded_event(func());
                record_replay::log(event, metadata.flavor, &metadata.type_name, &metadata.id);
                result
            }
            RecordReplayMode::Replay => {
                match get_det_id() {
                    None => {
                        // TODO Remove redundancy with NoRR, we probably wanna make this a
                        // top level check, and not record events when det_id is None?
                        // for now it's useful to verify that there are channels (and what
                        // _kinds_ of channels) are running with no det_id (probably because
                        // or running under rayon, but maybe because of a missing wrap around
                        // std::thread)
                        warn!("DetThreadId was None. This execution may not be deterministic.");
                        // Most call to inc_event_id happen in replay_recv(). Manually
                        // increase here!
                        inc_event_id();
                        func().map(|v| v.1)
                    }
                    Some(det_id) => {
                        match get_log_entry(det_id, get_event_id()) {
                            Some((event, flavor, id)) => {
                                if *flavor != metadata.flavor {
                                    panic!("Expected {:?}, saw {:?}", flavor, metadata.flavor);
                                }

                                if *id != metadata.id {
                                    panic!("Expected {:?}, saw {:?}", id, metadata.id);
                                }
                                self.expected_recorded_events(event)
                            }
                            None => {
                                // No need to inc_event_id here... This thread will hang
                                // forever.
                                if blocking == Blocking::CannotBlock {
                                    panic!(format!("Missing entry in log. A call to {} should never block!", function_name));
                                }
                                log_trace("No entry in log. Assuming this thread blocked on this select forever.");
                                // No entry in log. This means that this event waiting forever
                                // on this recv call.
                                loop {
                                    std::thread::park();
                                    log_trace("Spurious wakeup, going back to sleep.");
                                }
                            }
                        }
                    }
                }

            }
            RecordReplayMode::NoRR => func().map(|v| v.1),
        }
    }

    fn to_recorded_event(&self, event: Result<(Option<DetThreadId>, T), E>)
                         -> (Result<T, E>, Recorded);

    fn expected_recorded_events(&self, event: &Recorded)
                                -> Result<T, E>;
}

use std::cell::RefMut;
use std::collections::VecDeque;

pub fn replay_recv<T, E: Error>(
    expected_sender: &Option<DetThreadId>,
    recv: impl Fn() -> Result<(Option<DetThreadId>, T), E>,
    // TODO: Change from mut to &mut
    buffer: &mut RefMut<HashMap<DetThreadId, VecDeque<T>>>,
    none_buffer: &mut RefMut<VecDeque<T>>,
    id: &DetChannelId,
) -> T {
    log_trace_with(&format!("replay_recv(expected_sender: {:?}) ...", expected_sender), id);

    match expected_sender {
        None => {
            if let Some(entry) = none_buffer.pop_front() {
                log_trace_with("replay_recv(): Recv message found in none buffer.", id);
                return entry;
            }
        }
        Some(expected_sender) => {
            // Check our buffer to see if this value is already here.
            if let Some(entries) = buffer.get_mut(expected_sender) {
                if let Some(entry) = entries.pop_front() {
                    log_trace_with("replay_recv(): Recv message found in buffer.", id);
                    return entry;
                }
            }
        }
    }

    // Loop until we get the message we're waiting for. All "wrong" messages are
    // buffered into self.buffer.
    loop {
        match recv() {
            // Value matches return it!
            Ok((msg_sender, msg)) if msg_sender == *expected_sender => {
                log_trace("Recv message found through recv()");
                return msg;
            }
            // Value did no match. Buffer it. Handles both `none_buffer` and
            // regular `buffer` case.
            Ok((msg_sender, msg)) => {
                let wrong_val = &format!("Wrong value found. Queing it for: {:?}",
                                         msg_sender);
                log_trace_with(wrong_val, id);

                match msg_sender {
                    None => {
                        none_buffer.push_back(msg);
                    }
                    Some(msg_sender) => {
                        // ensure we don't drop temporary reference.
                        buffer.entry(msg_sender).
                            or_insert(VecDeque::new()).
                            push_back(msg);
                    }
                }

            }
            Err(e) => {
                log_trace(&format!("Saw Err({:?})", e));
                // Got a disconnected message. Ignore.
            }
        }
    }
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
    ) -> Result<(), E> {
        // Because of the router, we want to "forwad" the original sender's DetThreadId
        // sometimes. However, for the record log, we still want to use the original
        // thread's DetThreadId. Otherwise we will have "repeated" entries in the log
        // which look like they're coming from the same thread.
        let forwading_id = if in_forwarding() { get_temp_det_id() } else { get_det_id() };

        log_trace(&format!("{}<{:?}>::send(({:?}, {:?}))",
                           sender_name, id, forwading_id, type_name));

        match mode {
            // Record event.
            RecordReplayMode::Record => {
                let event = self.to_recorded_event(id.clone());
                // Note: send() must come before record_replay::log() as it internally
                // increments event_id.
                let result = self.send(forwading_id, msg);
                record_replay::log(event, flavor.clone(), type_name, id);
                result
            }
            // Ensure event is present and channel ids match.
            RecordReplayMode::Replay => {
                match get_det_id() {
                    // Nothing for us to check...
                    None => {
                        warn!("det_id is None. This execution may be nondeterministic");
                    }
                    Some(det_id) => {
                        let event = record_replay::get_log_entry(det_id, get_event_id());

                        // No sender event. This thread never got this far in record.
                        if event.is_none() {
                            log_trace("Putting thread to sleep!");
                            loop {
                                std::thread::park();
                                log_trace("Spurious wakeup, going back to sleep.");
                            }
                        }

                        let (recorded, sender_flavor, chan_id) = event.unwrap();
                        if sender_flavor != flavor {
                            panic!("Expected {:?}, saw {:?}", flavor, sender_flavor);
                        }

                        if chan_id != id {
                            panic!("Expected {:?}, saw {:?}", chan_id, id);
                        }

                        self.check_log_entry(recorded.clone());
                    }
                }
                let result = self.send(forwading_id, msg);
                inc_event_id();
                result
            }
            RecordReplayMode::NoRR => {
                // TODO. Should this use the forwading id as well?
                self.send(get_det_id(), msg)
            }
        }
    }

    fn check_log_entry(&self, entry: Recorded);

    fn send(&self, thread_id: Option<DetThreadId>, msg: T) -> Result<(), E>;

    fn to_recorded_event(&self, id: DetChannelId) -> Recorded;
}

