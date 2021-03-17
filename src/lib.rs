#![feature(trait_alias)]
#![feature(const_type_name)]

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[allow(unused_imports)]
use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

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

use std::io::Write;

use desync::DesyncMode;
use detthread::DetThreadId;
use std::env::var;
use std::env::VarError;
use std::sync::{Mutex, RwLock};

use crate::detthread::get_det_id;
pub use crate::detthread::init_tivo_thread_root;
use crate::recordlog::{RecordEntry, Recordable};
#[cfg(test)]
use crate::test::TEST_MODE;

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

pub enum LogImpl {
    InMemory,
    File,
}

fn get_recorder_impl() -> LogImpl {
    if cfg!(test) {
        LogImpl::InMemory
    } else {
        LogImpl::File
    }
}

lazy_static! {
    /// Singleton environment logger. Must be initialized somewhere, and only once.
    pub static ref ENV_LOGGER: () = {
        // env_logger::builder()
        //     .format_timestamp(None)
        //     .init()
    };

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
    pub static ref DESYNC_MODE: DesyncMode = {
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
    pub static ref LOG_FILE_NAME: String = {
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
    pub(crate) static ref IN_MEMORY_RECORDER:
     RwLock<HashMap<DetThreadId, Arc<Mutex<InMemoryRecorder>>>> = {
        // Init with initial entry. New entries are added by threads when they spawn.
        let mut hm = HashMap::new();
        let entry = Arc::new(Mutex::new(InMemoryRecorder::new(get_det_id())));
        hm.insert(get_det_id(), entry);
        RwLock::new(hm)
     };
}

thread_local! {
    static THREAD_LOCAL_RECORDER: Arc<Mutex<InMemoryRecorder>> = {
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

/// Record and Replay using a memory-based recorder. Useful for unit testing channel methods.
/// These derives are needed because IpcReceiver requires Ser/Des... sigh...
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub struct InMemoryRecorder {
    queue: VecDeque<RecordEntry>,
    /// The ID is the same for all events as this is a thread local record.
    /// TODO: this field isn't used... remove it?
    dti: DetThreadId,
}

pub(crate) fn set_global_memory_recorder(imr: HashMap<DetThreadId, InMemoryRecorder>) {
    let mut global_recorder = HashMap::new();
    for (k, v) in imr {
        global_recorder.insert(k, Arc::new(Mutex::new(v)));
    }

    *IN_MEMORY_RECORDER.try_write().unwrap() = global_recorder;
}

impl InMemoryRecorder {
    fn new(dti: DetThreadId) -> InMemoryRecorder {
        InMemoryRecorder {
            dti,
            queue: VecDeque::new(),
        }
    }

    fn add_entry(&mut self, r: RecordEntry) {
        self.queue.push_back(r);
    }

    fn get_next_entry(&mut self) -> Option<RecordEntry> {
        self.queue.pop_front()
    }
}

// Thread local in memory recorder. We have this "extra" struct for this because we want to
// implement the Recordable Trait for this type but not for InMemoryRecorder (since that would
// required all InMemoryRecorders to carry a refcell to get the types to match. We only want the
// refcell at the top level.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ThreadLocalIMR {}

impl ThreadLocalIMR {
    fn new() -> ThreadLocalIMR {
        ThreadLocalIMR {}
    }
}

impl Recordable for ThreadLocalIMR {
    fn write_entry(&self, entry: RecordEntry) {
        THREAD_LOCAL_RECORDER.with(|tlr| {
            tlr.lock().unwrap().add_entry(entry);
        });
    }

    fn next_record_entry(&self) -> Option<RecordEntry> {
        THREAD_LOCAL_RECORDER.with(|tlr| tlr.lock().unwrap().get_next_entry())
    }
}

pub(crate) fn get_global_memory_recorder() -> HashMap<DetThreadId, InMemoryRecorder> {
    let mut copy = HashMap::new();

    for (k, v) in IN_MEMORY_RECORDER.try_write().unwrap().iter() {
        copy.insert(k.clone(), v.lock().unwrap().clone());
    }
    copy
}

/// This is a mess... Originally I wanted to use a Box<dyn Recordable> and all channel receiver
/// and sender implementations would have a field with this type, unfortunately, this causes issues
/// and dynamic Trait objects cannot be easily serialized/deserialized... So we use an enum instead
/// ...
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum EventRecorder {
    Memory(ThreadLocalIMR),
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

    fn get_global_recorder() -> EventRecorder {
        match get_recorder_impl() {
            LogImpl::InMemory => EventRecorder::Memory(ThreadLocalIMR::new()),
            LogImpl::File => EventRecorder::File(FileRecorder::new()),
        }
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
    fn write_entry(&self, entry: RecordEntry) {
        use crate::recordlog::WRITE_LOG_FILE;

        let serialized = serde_json::to_string(&entry).unwrap();

        WRITE_LOG_FILE
            .lock()
            .expect("Unable to lock file.")
            .write_fmt(format_args!("{}\n", serialized))
            .expect("Unable to write to log file.");
    }

    fn next_record_entry(&self) -> Option<RecordEntry> {
        todo!()
        // let (re, cl, dci) = RECORDED_INDICES.get(key)?.clone();
        // Some(RecordEntry::new(
        //     key.0.clone(),
        //     key.1,
        //     re,
        //     cl,
        //     dci,
        //     "TODO".to_string(),
        // ))
    }
}
