use log::Level::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::remove_file;
use std::fs::File;
use std::io::BufRead;
use std::sync::mpsc;
use std::sync::Mutex;

use crate::detthread;
use crate::detthread::DetThreadId;
use crate::error;
use crate::error::IpcDummyError;
use crate::rr::DetChannelId;
use crate::{RRMode, LOG_FILE_NAME, RECORD_MODE};

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
    #[serde(with = "error::RecvErrorDef")]
    RecvErr(crossbeam_channel::RecvError),

    // MPSC recv.
    MpscRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "error::MpscRecvErrorDef")]
    MpscRecvErr(mpsc::RecvError),

    // Crossbeam try_recv
    TryRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "error::TryRecvErrorDef")]
    TryRecvErr(crossbeam_channel::TryRecvError),

    // MPSC try_recv
    MpscTryRecvSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "error::MpscTryRecvErrorDef")]
    MpscTryRecvErr(mpsc::TryRecvError),

    // Crossbeam recv_timeout
    RecvTimeoutSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "error::RecvTimeoutErrorDef")]
    RecvTimeoutErr(crossbeam_channel::RecvTimeoutError),

    // MPSC recv_timeout
    MpscRecvTimeoutSucc {
        sender_thread: Option<DetThreadId>,
    },
    #[serde(with = "error::MpscRecvTimeoutErrorDef")]
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

lazy_static::lazy_static! {
    /// Global log file which all threads write to.
    pub static ref WRITE_LOG_FILE: Mutex<File> = {
        crate::log_rr!(Debug, "Initializing WRITE_LOG_FILE lazy static.");

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
        crate::log_rr!(Debug, "Initializing RECORDED_INDICES lazy static.");
        use std::io::BufReader;

        let mut recorded_indices = HashMap::new();
        let log = File::open(LOG_FILE_NAME.as_str()).
            expect(& format!("Unable to open {} for replay.", LOG_FILE_NAME.as_str()));
        let log = BufReader::new(log);

        for line in log.lines() {
            let line = line.expect("Unable to read recorded log file");
            let entry: LogEntry = serde_json::from_str(&line).expect("Malformed log entry.");

            if entry.current_thread.is_none() {
                crate::log_rr!(Trace, "Skipping None entry in log for entry: {:?}", entry);
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
    crate::log_rr!(Debug, "rr::log()");
    use std::io::Write;

    // Write our (DET_ID, EVENT_ID) -> index to our log file
    let current_thread = detthread::get_det_id();
    let event_id = detthread::get_event_id();
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

    crate::log_rr!(
        Info,
        "{:?} Logged entry: {:?}",
        entry,
        (detthread::get_det_id(), event_id)
    );
    detthread::inc_event_id();
}

/// Actual implementation. Compares `curr_flavor` and `curr_id` against
/// log entry if they're Some(_).
fn get_log_entry_do<'a>(
    our_thread: DetThreadId,
    event_id: EventId,
    curr_flavor: Option<&ChannelLabel>,
    curr_id: Option<&DetChannelId>,
) -> Result<(&'a RecordedEvent, &'a ChannelLabel, &'a DetChannelId), error::DesyncError> {
    let key = (our_thread, event_id);
    match RECORDED_INDICES.get(&key) {
        Some((recorded, log_flavor, log_id)) => {
            crate::log_rr!(
                Debug,
                "Event fetched: {:?} for keys: {:?}",
                (recorded, log_flavor, log_id),
                key
            );

            if let Some(curr_flavor) = curr_flavor {
                if log_flavor != curr_flavor {
                    return Err(error::DesyncError::ChannelVariantMismatch(
                        *log_flavor,
                        *curr_flavor,
                    ));
                }
            }
            if let Some(curr_id) = curr_id {
                if log_id != curr_id {
                    return Err(error::DesyncError::ChannelMismatch(
                        log_id.clone(),
                        curr_id.clone(),
                    ));
                }
            }
            Ok((recorded, log_flavor, log_id))
        }
        None => Err(error::DesyncError::NoEntryInLog(key.0, key.1)),
    }
}

/// Get log entry and compare `curr_flavor` and `curr_id` with the values
/// on the record.
pub fn get_log_entry_with<'a>(
    our_thread: DetThreadId,
    event_id: EventId,
    curr_flavor: &ChannelLabel,
    curr_id: &DetChannelId,
) -> Result<&'a RecordedEvent, error::DesyncError> {
    get_log_entry_do(our_thread, event_id, Some(curr_flavor), Some(curr_id)).map(|(r, _, _)| r)
}

/// Get log entry with no comparison.
pub fn get_log_entry<'a>(
    our_thread: DetThreadId,
    event_id: EventId,
) -> Result<&'a RecordedEvent, error::DesyncError> {
    get_log_entry_do(our_thread, event_id, None, None).map(|(r, _, _)| r)
}

/// Get log entry returning the record, flavor, and channel id if needed.
pub fn get_log_entry_ret<'a>(
    our_thread: DetThreadId,
    event_id: EventId,
) -> Result<(&'a RecordedEvent, &'a ChannelLabel, &'a DetChannelId), error::DesyncError> {
    get_log_entry_do(our_thread, event_id, None, None)
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
