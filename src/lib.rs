use lazy_static::lazy_static;
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

pub mod crossbeam_channel;
mod crossbeam_select;
mod crossbeam_select_macro;
mod desync;
pub mod detthread;
mod error;
pub mod ipc_channel;
pub mod mpsc;
mod recordlog;
mod rr;

use desync::DesyncMode;
use detthread::DetThreadId;
use log::Level::*;
use std::env::var;
use std::env::VarError;

pub use crate::detthread::init_tivo_thread_root;
use crate::recordlog::{EventId, RecordEntry, Recordable};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RRMode {
    Record,
    Replay,
    NoRR,
}

/// To deterministically replay messages we pass our determininistic thread ID + the
/// original message.
pub type DetMessage<T> = (DetThreadId, T);

/// Every channel carries a buffer where message that shouldn't have arrived are stored.
pub type BufferedValues<T> = HashMap<DetThreadId, VecDeque<T>>;

const RECORD_MODE_VAR: &str = "RR_CHANNEL";
const DESYNC_MODE_VAR: &str = "RR_DESYNC_MODE";
const RECORD_FILE_VAR: &str = "RR_RECORD_FILE";

lazy_static! {
    /// Singleton environment logger. Must be initialized somewhere, and only once.
    pub static ref ENV_LOGGER: () = {
        // env_logger::builder()
        //     .format_timestamp(None)
        //     .init()
    };

    /// Record type. Initialized from environment variable RR_CHANNEL.
    pub static ref RECORD_MODE: RRMode = {
        crate::log_rr!(Debug, "Initializing RECORD_MODE lazy static.");

        let mode = match var(RECORD_MODE_VAR) {
            Ok(value) => {
                match value.as_str() {
                    "record" => RRMode::Record,
                    "replay" => RRMode::Replay,
                    "noRR"   => RRMode::NoRR,
                    e        => {
                        warn!("Unkown record and replay mode: {}. Assuming noRR.", e);
                        RRMode::NoRR
                    }
                }
            }
            Err(VarError::NotPresent) => RRMode::NoRR,
            Err(e @ VarError::NotUnicode(_)) => {
                warn!("RR_CHANNEL value is not valid unicode: {}, assuming noRR.", e);
                RRMode::NoRR
            }
        };

        crate::log_rr!(Info, "Mode {:?} selected.", mode);
        mode
    };

    /// Record type. Initialized from environment variable RR_CHANNEL.
    pub static ref DESYNC_MODE: DesyncMode = {
        crate::log_rr!(Debug, "Initializing DESYNC_MODE lazy static.");

        let mode = match var(DESYNC_MODE_VAR) {
            Ok(value) => {
                match value.as_str() {
                    "panic" => DesyncMode::Panic,
                    "keep_going" => DesyncMode::KeepGoing,
                    e => {
                        warn!("Unkown DESYNC mode: {}. Assuming keep_going.", e);
                        DesyncMode::KeepGoing
                    }
                }
            }
            Err(VarError::NotPresent) => DesyncMode::KeepGoing,
            Err(e @ VarError::NotUnicode(_)) => {
                warn!("DESYNC_MODE value is not valid unicode: {}, assuming keep_going.", e);
                DesyncMode::KeepGoing
            }
        };

        crate::log_rr!(Info, "Mode {:?} selected.", mode);
        mode
    };

    /// Name of record file.
    pub static ref LOG_FILE_NAME: String = {
        crate::log_rr!(Debug, "Initializing RECORD_FILE lazy static.");

        let mode = match var(RECORD_FILE_VAR) {
            Ok(value) => {
                value
            }
            Err(VarError::NotPresent) => {
                panic!("Unspecified record file. Please use env var RR_RECORD_FILE");
            }
            Err(e @ VarError::NotUnicode(_)) => {
                panic!("RECORD_FILE value is not valid unicode: {}.", e);
            }
        };

        crate::log_rr!(Info, "Mode {:?} selected.", mode);
        mode
    };
}

use crate::recordlog::RECORDED_INDICES;
use std::cell::RefCell;
use std::io::Write;

thread_local! {
    /// Our in-memory recorder
    static IN_MEMORY_RECORDER: RefCell<HashMap<(DetThreadId, u32), RecordEntry>> = RefCell::new(HashMap::new());
}

/// This is a mess... Originally I wanted to use a Box<dyn Recordable> and all channel receiver
/// and sender implementations would have a field with this type, unfortunately, this causes issues
/// and dynamic Trait objects cannot be easily serialized/deserialized... So we use an enum instead
/// ...
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum EventRecorder {
    Memory(InMemoryRecorder),
    File(FileRecorder),
}

impl EventRecorder {
    /// Easy method for turning our current variant of our log into a Recordable.
    fn get_recordable(&self) -> &dyn Recordable {
        match self {
            EventRecorder::Memory(m) => m,
            EventRecorder::File(f) => f,
        }
    }

    fn new_memory_recorder() -> EventRecorder {
        EventRecorder::Memory(InMemoryRecorder::new())
    }

    fn new_file_recorder() -> EventRecorder {
        EventRecorder::File(FileRecorder::new())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileRecorder {}

impl FileRecorder {
    fn new() -> FileRecorder {
        FileRecorder {}
    }
}

// TODO Test.
impl Recordable for FileRecorder {
    fn write_entry(&self, _id: DetThreadId, _event: EventId, entry: RecordEntry) {
        use crate::recordlog::WRITE_LOG_FILE;

        let serialized = serde_json::to_string(&entry).unwrap();

        WRITE_LOG_FILE
            .lock()
            .expect("Unable to lock file.")
            .write_fmt(format_args!("{}\n", serialized))
            .expect("Unable to write to log file.");
    }

    fn get_record_entry(&self, key: &(DetThreadId, u32)) -> Option<RecordEntry> {
        let (re, cl, dci) = RECORDED_INDICES.get(key)?.clone();
        Some(RecordEntry::new(
            key.0.clone(),
            key.1,
            re,
            cl,
            dci,
            "TODO".to_string(),
        ))
    }
}

/// Record and Replay using a memory-based recorder. Useful for unit testing channel methods.
// These derives are needed because IpcReceiver requires Ser/Des... sigh...
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InMemoryRecorder {}

impl Recordable for InMemoryRecorder {
    fn write_entry(&self, id: DetThreadId, event: EventId, entry: RecordEntry) {
        IN_MEMORY_RECORDER.with(|ml| {
            ml.borrow_mut().insert((id, event), entry);
        })
    }

    fn get_record_entry(&self, key: &(DetThreadId, u32)) -> Option<RecordEntry> {
        IN_MEMORY_RECORDER.with(|ml| ml.borrow().get(key).cloned())
    }
}

impl InMemoryRecorder {
    fn new() -> InMemoryRecorder {
        InMemoryRecorder {}
    }
}

#[macro_export]
/// Log messages with added information. Specifically:
/// thread name, event_name, envent_id, correct name resolution when forwading.
macro_rules! log_rr {
    ($log_level:expr, $msg:expr, $($arg:expr),*) => {

        let thread = std::thread::current();
        let formatted_msg = format!($msg, $($arg),*);
        log::log!($log_level,
             "thread: {:?} | event# {:?} {} | {}",
             thread.name(),
             crate::event_name(),
             crate::detthread::get_event_id(),
             formatted_msg);
    };
    ($log_level:expr, $msg:expr) => {
        crate::log_rr!($log_level, $msg,);
    };
}

/// Helper function to `log_rr` macro. Handles log printing from router
/// properly.
fn event_name() -> String {
    if crate::detthread::in_forwarding() {
        "ROUTER".to_string()
    } else {
        format!("{:?}", (detthread::get_det_id(), detthread::get_event_id()))
    }
}
