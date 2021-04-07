#![feature(trait_alias)]
#![feature(const_type_name)]

use std::collections::{HashMap, VecDeque};
use std::env::var;
use std::env::VarError;
use std::sync::Arc;
use std::sync::{Mutex, RwLock};

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

use desync::DesyncMode;
use detthread::DetThreadId;
use recordlog::InMemoryRecorder;

use crate::detthread::get_det_id;
pub use crate::detthread::init_tivo_thread_root;
use crate::error::DesyncError;
use crate::recordlog::{RecordEntry, RecordMetadata, RecordedEvent};
#[cfg(test)]
use crate::test::TEST_MODE;

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
#[cfg(test)]
pub mod test;
mod utils;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RRMode {
    Record,
    Replay,
    NoRR,
}

/// To deterministically replay messages we pass our deterministic thread ID + the
/// original message.
pub type DetMessage<T> = (DetThreadId, T);

/// Every channel carries a buffer where message that shouldn't have arrived are stored.
pub type BufferedValues<T> = HashMap<DetThreadId, VecDeque<T>>;

const RECORD_MODE_VAR: &str = "RR_CHANNEL";
const DESYNC_MODE_VAR: &str = "RR_DESYNC_MODE";
const RECORD_FILE_VAR: &str = "RR_RECORD_FILE";

#[cfg(test)]
fn get_rr_mode() -> RRMode {
    TEST_MODE
        .lock()
        .unwrap()
        .expect("TEST_MODE not initialized when running tests!")
}

#[cfg(not(test))]
fn get_rr_mode() -> RRMode {
    *RECORD_MODE
}

lazy_static! {
    /// Record type. Initialized from environment variable RR_CHANNEL.
    static ref RECORD_MODE: RRMode = {
        debug!("Initializing RECORD_MODE lazy static.");

        let mode = match var(RECORD_MODE_VAR) {
            Ok(value) => {
                match value.as_str() {
                    "record" => RRMode::Record,
                    "replay" => RRMode::Replay,
                    "noRR"   => RRMode::NoRR,
                    e        => panic!("Unknown record and replay mode: {}", e),
                }
            }
            Err(_) => panic!("Please specify RR mode via {}", RECORD_MODE_VAR),
        };

        info!("Mode {:?} selected.", mode);
        mode
    };

    /// Record type. Initialized from environment variable RR_CHANNEL.
    static ref DESYNC_MODE: DesyncMode = {
        debug!("Initializing DESYNC_MODE lazy static.");

        let mode = match var(DESYNC_MODE_VAR) {
            Ok(value) => {
                match value.as_str() {
                    "panic" => DesyncMode::Panic,
                    "keep_going" => DesyncMode::KeepGoing,
                    e => {
                        warn!("Unkown DESYNC mode: {}. Assuming panic.", e);
                        DesyncMode::Panic
                    }
                }
            }
            Err(VarError::NotPresent) => DesyncMode::Panic,
            Err(e @ VarError::NotUnicode(_)) => {
                warn!("DESYNC_MODE value is not valid unicode: {}, assuming panic.", e);
                DesyncMode::Panic
            }
        };

        error!("DesyncMode {:?} selected.", mode);
        mode
    };

    /// Name of record file.
    static ref LOG_FILE_NAME: String = {
        debug!("Initializing RECORD_FILE lazy static.");

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

        info!("Mode {:?} selected.", mode);
        mode
    };

    /// Global in-memory recorder for events.
    /// See Issue #46 for more information.
    static ref IN_MEMORY_RECORDER:
     RwLock<HashMap<DetThreadId, Arc<Mutex<InMemoryRecorder>>>> = {
        // Init with initial entry. New entries are added by threads when they spawn.
        let mut hm = HashMap::new();
        let entry = Arc::new(Mutex::new(InMemoryRecorder::new(get_det_id())));
        hm.insert(get_det_id(), entry);
        RwLock::new(hm)
     };
}

thread_local! {
    /// Every thread has a reference to its own recorder. This is to avoid too much contention if all
    /// threads were trying to write to the global IN_MEMORY_RECORDER.
    static THREAD_LOCAL_RECORDER: Arc<Mutex<InMemoryRecorder>> = {
            // TODO: Should we be fetching the dti every time?
            let detid = get_det_id();
            let imr = IN_MEMORY_RECORDER
                .write()
                .expect("Unable to acquire rwlock");

            match imr.get(&detid) {
                Some(imr) => imr.clone(),
                None => panic!("Unable to get record log for {:?}. Available entries: {:?}.", detid, imr.keys()),
            }
    };

    static MAIN_THREAD_SPAN: EnteredSpan = {
        span!(Level::INFO, "Thread", dti=?get_det_id()).entered()
    }
}

/// The actual IN_MEMORY_RECORDER has a much different type (includes a few locks and Arcs to play
/// nicely with Rust's type system) this function allows us to init the global recorder using a
/// much simpler type and takes care of the conversion.
pub(crate) fn set_global_memory_recorder(imr: HashMap<DetThreadId, InMemoryRecorder>) {
    let mut global_recorder = HashMap::new();
    for (k, v) in imr {
        global_recorder.insert(k, Arc::new(Mutex::new(v)));
    }

    *IN_MEMORY_RECORDER.try_write().unwrap() = global_recorder;
}

pub(crate) fn get_global_memory_recorder() -> HashMap<DetThreadId, InMemoryRecorder> {
    let mut copy = HashMap::new();

    for (k, v) in IN_MEMORY_RECORDER.try_write().unwrap().iter() {
        copy.insert(k.clone(), v.lock().unwrap().clone());
    }
    copy
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct EventRecorder {}

impl EventRecorder {
    fn get_global_recorder() -> EventRecorder {
        EventRecorder {}
    }

    fn next_record_entry(&self) -> Option<RecordEntry> {
        THREAD_LOCAL_RECORDER.with(|tlr| tlr.lock().unwrap().get_next_entry())
    }

    fn write_event_to_record(&self, event: RecordedEvent, metadata: &RecordMetadata) {
        debug!("rr::log()");

        let entry: RecordEntry = RecordEntry {
            event,
            channel_variant: metadata.channel_variant,
            chan_id: metadata.id.clone(),
            type_name: metadata.type_name.clone(),
        };

        THREAD_LOCAL_RECORDER.with(|tlr| {
            tlr.lock().unwrap().add_entry(entry.clone());
        });

        warn!("Logged entry: {:?}", entry);
    }

    /// Get log entry returning the record, variant, and channel ID.
    fn get_log_entry(&self) -> desync::Result<RecordEntry> {
        self.next_record_entry().ok_or_else(|| {
            let error = DesyncError::NoEntryInLog;
            warn!(%error);
            error
        })
    }
}

// impl Recordable for FileRecorder {
//     fn write_entry(&self, entry: RecordEntry) {
//         use crate::recordlog::WRITE_LOG_FILE;
//
//         let serialized = serde_json::to_string(&entry).unwrap();
//
//         WRITE_LOG_FILE
//             .lock()
//             .expect("Unable to lock file.")
//             .write_fmt(format_args!("{}\n", serialized))
//             .expect("Unable to write to log file.");
//     }
//
//     fn next_record_entry(&self) -> Option<RecordEntry> {
//         todo!()
//         // let (re, cl, dci) = RECORDED_INDICES.get(key)?.clone();
//         // Some(RecordEntry::new(
//         //     key.0.clone(),
//         //     key.1,
//         //     re,
//         //     cl,
//         //     dci,
//         //     "TODO".to_string(),
//         // ))
//     }
// }
