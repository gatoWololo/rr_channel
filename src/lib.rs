use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::thread::JoinHandle;
use std::collections::HashMap;

use lazy_static::lazy_static;
use std::fs::{remove_file, File};

pub mod crossbeam_select;

/// A singleton instance exists globally for the current mode via lazy_static global variable.
#[derive(Clone, Copy)]
pub enum RecordReplayMode {
    Record,
    Replay,
    NoRR,
}

// TODO use environment variables to generalize this.
const LOG_FILE_NAME: &str = "/home/gatowololo/det_file.txt";
const ENV_VAR_NAME: &str = "RR_CHANNEL";

lazy_static! {
    /// Record type. Initialized from environment variable RR_CHANNEL.
    pub static ref RECORD_MODE: RecordReplayMode = {
        use std::env::var;
        use std::env::VarError;
        match var(ENV_VAR_NAME) {
            Ok(value) => {
                match value.as_str() {
                    "record" => RecordReplayMode::Record,
                    "replay" => RecordReplayMode::Replay,
                    "none"   => RecordReplayMode::NoRR,
                    e        => panic!("Unkown record and replay mode: {}", e),
                }
            }
            Err(VarError::NotPresent) => RecordReplayMode::NoRR,
            Err(e @ VarError::NotUnicode(_)) => panic!("RR_CHANNEL value is not valid unicode: {}", e),
        }
    };

    /// Global log file which all threads write to.
    pub static ref LOG_FILE: Mutex<File> = {
        match *RECORD_MODE {
            RecordReplayMode::Record => {
                // Delete file if it already existed... file may not exist. That's okay.
                let _ = remove_file(LOG_FILE_NAME);
                let error = & format!("Unable to open {} for record logging.", LOG_FILE_NAME);
                Mutex::new(File::create(LOG_FILE_NAME).expect(error))
            }
            RecordReplayMode::Replay => {
                let error = & format!("Unable to open {} for replay.", LOG_FILE_NAME);
                Mutex::new(File::open(LOG_FILE_NAME).expect(error))
            }
            RecordReplayMode::NoRR =>
                panic!("\"None\" record and replay option found. We should be be opening the log."),
        }
    };

    /// Global map holding all indexes from the record phase. Lazily initialized on replay mode.
    /// Maps (DET_ID, SELECT_ID) -> index.
    /// TODO: Use type aliases to show this.
    pub static ref RECORDED_INDICES: HashMap<(usize, usize), usize> = {
        // let log_file: &mut () = LOG_FILE.lock().unwrap();
        // for entry in log_file {
        // }
        unimplemented!()
    };
}

#[macro_export]
macro_rules! rr_select {
    ($($tokens:tt)*) => {
        use rr_channels::{LOG_FILE, RECORD_MODE, get_det_id, get_select_id, RecordReplayMode, inner_select};
        use std::io::Write;

        match *RECORD_MODE {
            // Recording, get index from crossbeam_channel::select! and
            // record it in our log.
            // TODO: Writing everytime to file is probably slow? Should we buffer our
            // logging? Or is the std library logging good enough?
            RecordReplayMode::Record => {
                // Wrap in closure to allow returning values from macro.
                let index: usize = (|| inner_select!($($tokens)*))();

                // Write our (DET_ID, SELECT_ID) -> index to our log file
                LOG_FILE.
                    lock().
                    unwrap().
                    write_fmt(format_args!("{} {} {}\n", get_det_id(), get_select_id(), index));
            }
            // Query our log to see what index was selected!() during the replay phase.
            RecordReplayMode::Replay => {
                unimplemented!()
            }
            // Call unmodified select from crossbeam.
            RecordReplayMode::NoRR => crossbeam_channel::select!($($tokens)*),
        }
    }
}

/// Global counter for unique thread IDs.
/// TODO: this will not work once we have races for thread creation, good enough
/// for now...
static DET_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Get a unique ID for this thread. Internally mutates global counter.
/// TODO This should be mut somehow.
pub fn get_unique_det_id() -> usize {
    DET_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

pub fn get_det_id() -> usize {
    DET_ID.with(|id| *id.borrow())
}

pub fn get_select_id() -> usize {
    SELECT_ID.with(|id| *id.borrow())
}

thread_local! {
    /// TODO this id is probably redundant.
    static SELECT_ID: RefCell<usize> = RefCell::new(0);
    /// Unique threadID assigned at thread spawn to to each thread.
    /// We can query this "globaly" for our current thread.
    static DET_ID: RefCell<usize> = RefCell::new(0);
}

/// Wrapper around thread::spawn. We will need this later to assign a
/// deterministic thread id, and allow each thread to have access to its ID and
/// a local dynamic select counter through TLS.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    thread::spawn(|| {
        // Initialize TLS for this thread. Specifically, our select macro will need to
        // fetch this thread's det thread id.
        DET_ID.with(|det_id| {
            *det_id.borrow_mut() = get_unique_det_id();
        });
        // SELECT_ID is fine starting at 0.
        f()
    })
}
