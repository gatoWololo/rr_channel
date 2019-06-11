use std::cell::RefCell;
use std::collections::HashMap;
use std::io::prelude::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread;
use std::thread::JoinHandle;

use lazy_static::lazy_static;
use std::fs::{remove_file, File};

use env_logger;
use log::{debug, trace};

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

/// Global counter for unique thread IDs.
/// TODO: this will not work once we have races for thread creation, good enough
/// for now...
static DET_ID_COUNTER: AtomicU32 = AtomicU32::new(0);

lazy_static! {
    /// Singleton environment logger. Must be initialized somewhere, and only once.
    pub static ref ENV_LOGGER: () = {
        // env_logger::init();
        // Init logger with no timestamp data or module name.
        env_logger::Builder::from_default_env().
            default_format_timestamp(false).
            default_format_module_path(false).
            init();
    };
    /// Record type. Initialized from environment variable RR_CHANNEL.
    pub static ref RECORD_MODE: RecordReplayMode = {
        trace!("Initializing RECORD_MODE lazy static.");
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
            Err(e @ VarError::NotUnicode(_)) =>
                panic!("RR_CHANNEL value is not valid unicode: {}", e),
        }
    };

    /// Global log file which all threads write to.
    pub static ref WRITE_LOG_FILE: Mutex<File> = {
        trace!("Initializing WRITE_LOG_FILE lazy static.");

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
    /// Maps (DET_ID, SELECT_ID) -> index.
    /// TODO: Use type aliases to show this.
    pub static ref RECORDED_INDICES: HashMap<(u32, u32), u32> = {
        trace!("Initializing RECORDED_INDICES lazy static.");
        use std::io::BufReader;

        let mut recorded_indices = HashMap::new();
        let log = File::open(LOG_FILE_NAME).
            expect(& format!("Unable to open {} for replay.", LOG_FILE_NAME));
        let log = BufReader::new(log);

        for line in log.lines() {
            let line = line.expect("Unable to read recorded log file");
            let vals: Vec<u32> = line.split(" ").
                map(|c|c.parse().expect("Unable to parse log.")).collect();

            if let [det_id, select_id, index] = *vals.as_slice() {
                recorded_indices.insert((det_id, select_id), index);
            } else {
                panic!("Malformed log entry: {}", line);
            }
        }
        trace!("{:?}", recorded_indices);
        recorded_indices
    };
}

/// Given the token stream for a crossbeam::select!() and the index indetifier $index,
/// generate a match statement on the index calling the appropriate channel and code
/// to execute.
#[macro_export]
macro_rules! make_replay {
    // Parse input as expected by crossbeam_channel::select!().
    // Expects body to be wrapper in curly braces.
    ($index:ident, $case:expr, recv($r:expr) -> $res:pat => $body:tt, $($tail:tt)*) => {
        if $index == $case {
            let $res = $r.recv();
            $body;
            return;
        }
        make_replay!($index, $case + 1u32, $($tail)*);
    };
    // $body was **not** wrapped in curly braces. Wrap here and call recursively.
    ($index:ident, $case:expr, recv($r:expr) -> $res:pat => $body:expr, $($tail:tt)*) => {
        make_replay!($index, $case, recv($r) -> $res => $body, $($tail)*);
    };
    ($index:ident, $case:expr, ) => {
        // All done!
    };
    ($anything:tt) => {
        compile_error!("Could not parse expression. Something went wrong.");
    }
}

#[macro_export]
macro_rules! rr_select {
    ($($tokens:tt)*) => {
        use rr_channels::{WRITE_LOG_FILE, RECORD_MODE, get_det_id, get_select_id, RecordReplayMode, inner_select, RECORDED_INDICES, ENV_LOGGER, make_replay, inc_select_id};
        use std::io::Write;
        use log::{trace, debug};
        // Init log, happens once, lazily.
        *ENV_LOGGER;

        match *RECORD_MODE {
            // Recording, get index from crossbeam_channel::select! and
            // record it in our log.

            // TODO: Writing everytime to file is probably slow? Should we buffer our
            // logging? Or is the std library logging good enough?
            RecordReplayMode::Record => {
                // Wrap in closure to allow returning values from macro.
                let index: u32 = (|| {inner_select!( $($tokens)* )})() as u32;
                // Write our (DET_ID, SELECT_ID) -> index to our log file
                WRITE_LOG_FILE.
                    lock().
                    unwrap().
                    write_fmt(format_args!("{} {} {}\n", get_det_id(), get_select_id(), index));
                debug!("det_id: {}, select_id: {}, index: {}", get_det_id(), get_select_id(), index);
                inc_select_id();
            }
            // Query our log to see what index was selected!() during the replay phase.
            RecordReplayMode::Replay => {
                let key = (get_det_id(), get_select_id());
                trace!("Replaying for key {:?}", key);

                let index = *RECORDED_INDICES.get(& key).expect("Unable to fetch key.");
                trace!("Index fetched: {}", index);

                // TODO Make closure part of the macro!
                (|| {
                    make_replay!(index, 0u32, $($tokens)*);
                })();

                // Increase dynamic select!() count.
                inc_select_id();
            }
            // Call unmodified select from crossbeam.
            RecordReplayMode::NoRR => crossbeam_channel::select!($($tokens)*),
        }
    }
}

/// Get a unique ID for this thread. Internally mutates global counter.
/// TODO This should be mut somehow.
pub fn get_unique_det_id() -> u32 {
    DET_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

pub fn get_det_id() -> u32 {
    DET_ID.with(|id| *id.borrow())
}

/// TODO Mark as mutable somewhere?
pub fn get_select_id() -> u32 {
    SELECT_ID.with(|id| { *id.borrow() })
}

pub fn inc_select_id() {
    SELECT_ID.with(|id| {
        *id.borrow_mut() += 1;
    });
}

thread_local! {
    /// TODO this id is probably redundant.
    static SELECT_ID: RefCell<u32> = RefCell::new(0);
    /// Unique threadID assigned at thread spawn to to each thread.
    /// We can query this "globaly" for our current thread.
    static DET_ID: RefCell<u32> = RefCell::new(0);
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
        DET_ID.with(|id| {
            *id.borrow_mut() = get_unique_det_id();
            trace!("Assigned DET_ID {} for new thread.", *id.borrow());
        });
        // SELECT_ID is fine starting at 0.
        f()
    })
}
