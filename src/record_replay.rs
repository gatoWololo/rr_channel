use crate::DetThreadId;
use crate::RecordReplayMode;
use crate::RECORD_MODE;
use lazy_static::lazy_static;
use std::fs::remove_file;
use std::fs::File;
use std::sync::Mutex;

use crate::log_trace;
use crate::thread::get_det_id;
use crate::thread::get_event_id;
use crate::thread::inc_event_id;
use crossbeam_channel::{RecvError, RecvTimeoutError, TryRecvError};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;

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

    // IpcRouterSelectSucc {
    //     select_indices: Vec<u64>,
    // }
    // IpcRouterSelectErr,
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
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (Recorded, FlavorMarker)> = {
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
            let value = (entry.event.clone(), entry.channel);

            let prev = recorded_indices.insert(key, value);
            if prev.is_some() {
                panic!("Failed to replay. Adding key-value ({:?}, {:?}) but previous value \
                        {:?} already exited. Hashmap entries should be unique.",
                       (entry.current_thread, entry.event_id),
                       (entry.event, entry.channel),
                       prev);
            }

        }

        // log_trace("{:?}", recorded_indices);
        recorded_indices
    };
}

/// Unwrap rr_channel return value from calling .recv() and log results. Should only
/// be called in Record Mode.
pub fn log(event: Recorded, channel: FlavorMarker, type_name: &str) {
    log_trace("record_replay::log()");
    use std::io::Write;

    // Write our (DET_ID, EVENT_ID) -> index to our log file
    let current_thread = get_det_id();
    let event_id = get_event_id();
    let tid = format!("{:?}", ::std::thread::current().id());
    let pid = std::process::id();
    let entry: LogEntry = LogEntry {
        current_thread,
        event_id,
        event,
        channel,
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
) -> Option<&'a (Recorded, FlavorMarker)> {
    let key = (our_thread, event_id);
    let log_entry = RECORDED_INDICES.get(&key);

    log_trace(&format!(
        "Event fetched: {:?} for keys: {:?}",
        log_entry, key
    ));
    log_entry
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetChannelId {
    pub det_thread_id: Option<DetThreadId>,
    pub channel_id: u32
}
pub type EventId = u32;

#[derive(Debug, Serialize, Deserialize)]
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

use crate::channel::get_log_event;
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
                record_replay::log(event, metadata.flavor, &metadata.type_name);
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
                        func().map(|v| v.1)
                    }
                    Some(det_id) => {
                        match get_log_entry(det_id, get_event_id()) {
                            Some((event, flavor)) => {
                                inc_event_id();
                                if *flavor != metadata.flavor {
                                    panic!("Expected {:?}, saw {:?}", flavor, metadata.flavor);
                                }
                                self.expected_recorded_events(event)
                            }
                            None => {
                                if blocking == Blocking::CannotBlock {
                                    panic!(format!("Missing entry in log. A call to {} should never block!", function_name));
                                }
                                inc_event_id();
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

    fn replay_recv(&self, sender: &DetThreadId) -> T;
}

use std::cell::RefMut;
use std::collections::VecDeque;

pub fn replay_recv<T, E: Error>(
    sender: &DetThreadId,
    mut buffer: RefMut<HashMap<DetThreadId, VecDeque<T>>>,
    recv: impl Fn() -> Result<(Option<DetThreadId>, T), E>,
) -> T {
    log_trace("replay_recv()");
    // Check our buffer to see if this value is already here.
    if let Some(entries) = buffer.get_mut(sender) {
        if let Some(entry) = entries.pop_front() {
            log_trace("Recv message found in buffer.");
            return entry;
        }
    }

    // Loop until we get the message we're waiting for. All "wrong" messages are
    // buffered into self.buffer.
    loop {
        match recv() {
            Ok((None, msg)) => panic!("replay_recv"),
            Ok((Some(det_id), msg)) => {
                // We found the message from the expected thread!
                if det_id == *sender {
                    log_trace("Recv message found through recv()");
                    return msg;
                }
                // We got a message from the wrong thread.
                else {
                    log_trace(&format!("Wrong value found. Queing value for thread: {:?}", det_id));
                    let queue = buffer.entry(det_id).or_insert(VecDeque::new());
                    queue.push_back(msg);
                }
            }
            Err(e) => {
                log_trace(&format!("Saw Err({:?})", e));
                // Got a disconnected message. Ignore.
            }
        }
    }
}
