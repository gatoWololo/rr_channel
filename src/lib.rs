#![feature(bind_by_move_pattern_guards)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::prelude::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread;
use std::thread::JoinHandle;

use lazy_static::lazy_static;
use std::fs::{remove_file, File};

use env_logger;
use log::{trace, debug};

pub mod crossbeam_select;
pub mod thread_tree;
use thread_tree::{DetThreadId, DetIdSpawner};
use serde::{Serialize, Deserialize};

/// A singleton instance exists globally for the current mode via lazy_static global variable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RecordReplayMode {
    Record,
    Replay,
    NoRR,
}

/// Unique Identifier for entries in our log. Useful for easy serialize/deserialize
/// into our log.
#[derive(Serialize, Deserialize, Debug)]
pub struct LogEntry {
    pub current_thread: DetThreadId,
    /// For multiple producer channels we need to diffentiate who the sender was.
    /// As the index only tells us the correct receiver end, but not who the sender was.
    pub sender_thread: Option<DetThreadId>,
    pub select_id: u32,
    pub index: u32
}

use crossbeam_channel::SendError;
use crossbeam_channel::RecvError;

#[derive(Clone)]
pub struct Sender<T>{
    pub sender: crossbeam_channel::Sender<(Option<DetThreadId>, T)>,
    mode: RecordReplayMode,
}


impl<T> Sender<T> {
    pub fn send(&self, msg: T) -> Result<(), SendError<T>> {
        match self.mode {
            RecordReplayMode::Replay | RecordReplayMode::Record => {
                let det_id = Some(get_det_id());
                trace!("Sending our id via channel: {:?}", det_id);
                self.sender.send((det_id, msg)).
                    // crossbeam.send() returns Result<(), SendError<(DetThreadId, T)>>,
                    // we want to make this opaque to the user. Just return the T on error.
                    map_err(|e| SendError(e.into_inner().1))
            }
            RecordReplayMode::NoRR => {
                self.sender.send((None, msg)).
                    map_err(|e| SendError(e.into_inner().1))
            }
        }

    }
}

pub struct Receiver<T>{
    // /// Should be set before calling recv(). Will panic if not set.
    // /// We do not return an error to keep apperances with crossbeam_channels
    // /// API.
    // expected_id: Option<DetThreadId>,
    /// We could buffer multiple values for a given thread.
    buffer: HashMap<DetThreadId, VecDeque<T>>,
    pub receiver: crossbeam_channel::Receiver<(Option<DetThreadId>, T)>,
    mode: RecordReplayMode,
}

impl<T> Receiver<T> {
    // pub fn set_expected_id(&mut self, expected_id: DetThreadId) {
    //     if self.mode != RecordReplayMode::Replay {
    //         panic!("Expected id only makes sense in Replay mode");
    //     }
    //     self.expected_id = Some(expected_id);
    // }
    pub fn recv(&mut self) -> Result<(DetThreadId, T), RecvError> {
                if self.mode != RecordReplayMode::Record {
            panic!("record_recv should only be called in record mode.");
        }
        self.receiver.recv().
            map(|(id, msg)| (id.expect("None sent through channel on record."), msg))
    }

    pub fn replay_recv(&mut self, sender: &Option<DetThreadId>) -> Result<T, RecvError> {
        // Expected an error, just fake it like we got the error.
        if sender.is_none() {
            trace!("User waiting fro RecvError. Created artificial error.");
            return Err(RecvError);
        }

        let sender: &_ = sender.as_ref().unwrap();
        // Check our buffer to see if this value is already here.
        if let Some(entries) = self.buffer.get_mut(sender) {
            if let Some(entry) = entries.pop_front() {
                debug!("Recv message found in buffer.");
                return Ok(entry);
            }
        }

        // Nope, keep receiving until we find it.
        loop {
            match self.receiver.recv() {
                Ok((det_id, msg)) => {
                    let det_id = det_id.
                        expect("None was sent as det_id in record/replay mode!");

                    // We found the message from the expected thread!
                    if det_id == *sender {
                        debug!("Recv message found through recv()");
                        return Ok(msg);
                    }
                    // We got a message from the wrong thread.
                    else {
                        debug!("Wrong value found. Queing value for thread: {:?}", det_id);
                        let queue = self.buffer.entry(det_id).or_insert(VecDeque::new());
                        queue.push_back(msg);
                    }
                }
               Err(_) => {
                   // Got a disconnected message. Ignore.
                }
            }
        }
    }
}

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    // Init log, happens once, lazily.
    // Users always have to make channels before using the rest of this code.
    // So we put it here.
    *ENV_LOGGER;

    let (sender, receiver) = crossbeam_channel::unbounded();
    let mode = *RECORD_MODE;

    (Sender {sender, mode },
     Receiver{buffer: HashMap::new(), receiver, mode })
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
    /// Maps (our_thread: DetThreadId, SELECT_ID: u32) ->
    ///      (index: u32, sender_thread: DetThreadId)
    /// TODO: Use type aliases to show this.
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (u32, Option<DetThreadId>)> = {
        trace!("Initializing RECORDED_INDICES lazy static.");
        use std::io::BufReader;

        let mut recorded_indices = HashMap::new();
        let log = File::open(LOG_FILE_NAME).
            expect(& format!("Unable to open {} for replay.", LOG_FILE_NAME));
        let log = BufReader::new(log);

        for line in log.lines() {
            let line = line.expect("Unable to read recorded log file");
            let entry: LogEntry = serde_json::from_str(&line).
                expect("Malformed log entry.");
            recorded_indices.insert((entry.current_thread, entry.select_id),
                                    (entry.index, entry.sender_thread));
        }

        trace!("{:?}", recorded_indices);
        recorded_indices
    };
}

/// Given the token stream for a crossbeam::select!() and the index indetifier $index,
/// generate a match statement on the index calling the appropriate channel and code
/// to execute.
/// It basically turns code of the form:
///
///  rr_channels::rr_select! {
///    recv(r1) -> x => println!("receiver 1: {:?}", x),
///    recv(r2) -> y => println!("receiver 2: {:?}", y),
///  }
///
/// Into:
///
///  if(index == 0){
///    let x = r1.recv();
///    println!("receiver 1: {:?}", x),
///    return;
///  }
///  if(index == 1){
///    let y = r2.recv();
///    println!("receiver 2: {:?}", y),
///    return;
///  }
///
/// In the future we may generalize it to accept more patterns.
#[macro_export]
macro_rules! make_replay {
    // Parse input as expected by crossbeam_channel::select!().
    // Expects body to be wrapper in curly braces.
    ($index:ident, $sender_id:ident, $case:expr, recv($r:expr) -> $res:pat => $body:tt, $($tail:tt)*) => {
        if $index == $case {
            let $res = $r.replay_recv(& $sender_id);
            $body;
            return;
        }
        make_replay!($index, $sender_id, $case + 1u32, $($tail)*);
    };
    // $body was **not** wrapped in curly braces. Call recursively.
    // TODO this case may be redundant...
    ($index:ident, $sender_id:ident, $case:expr, recv($r:expr) -> $res:pat => $body:expr, $($tail:tt)*) => {
        make_replay!($index, $sender_id, $case, recv($r) -> $res => $body, $($tail)*);
    };
    ($index:ident, $sender_id:ident, $case:expr, ) => {
        // All done!
    };
    ($anything:tt) => {
        compile_error!("Could not parse expression. Something went wrong.");
    }
}

#[macro_export]
macro_rules! rr_select {
    ($($tokens:tt)*) => {
        use rr_channels::{WRITE_LOG_FILE, RECORD_MODE, get_det_id, get_select_id,
                          RecordReplayMode, inner_select, RECORDED_INDICES, ENV_LOGGER,
                          make_replay, inc_select_id, LogEntry};
        use rr_channels::thread_tree::DetThreadId;
        use std::io::Write;
        use log::{trace, debug};

        match *RECORD_MODE {
            // Recording, get index from crossbeam_channel::select! and
            // record it in our log.

            // TODO: Writing everytime to file is probably slow? Should we buffer our
            // logging? Or is the std library logging good enough?
            RecordReplayMode::Record => {
                // Wrap in closure to allow easily returning values from macro.
                // TODO If body of select statement has their own return, our code will
                // not work correctly.
                let (index, sender_thread) = (|| -> (u32, Option<DetThreadId>)
                                              {inner_select!( $($tokens)* )})();

                // Write our (DET_ID, SELECT_ID) -> index to our log file
                let current_thread = get_det_id();
                let select_id = get_select_id();
                let entry: LogEntry = LogEntry { current_thread, sender_thread,
                                                 select_id , index };
                let serialized = serde_json::to_string(&entry).unwrap();

                WRITE_LOG_FILE.
                    lock().
                    unwrap().
                    write_fmt(format_args!("{}\n", serialized)).
                    expect("Unable to write to log file.");

                debug!("Logged entry: {:?}", entry);
                inc_select_id();
            }
            // Query our log to see what index was selected!() during the replay phase.
            RecordReplayMode::Replay => {
                let key = (get_det_id(), get_select_id());
                trace!("Replaying for our_thread: {:?}, sender_thread {:?}",
                       key.0, key.1);
                let (index, sender_id) = RECORDED_INDICES.get(& key).
                    expect("Unable to fetch key.");
                trace!("Index fetched: {}", index);
                trace!("Sender Id fetched: {:?}", sender_id);
                let sender_id = sender_id.clone();
                let index = *index;
                // TODO Make closure part of the macro!
                // TODO Do not clone sender_id consume it.
                (|| {
                    make_replay!(index, sender_id, 0u32, $($tokens)*);
                })();

                // Increase dynamic select!() count.
                inc_select_id();
            }
            // Call unmodified select from crossbeam.
            // RecordReplayMode::NoRR => crossbeam_channel::select!($($tokens)*),
            RecordReplayMode::NoRR =>
                panic!("NoRR not impleneted. Please pick either record or replay"),
        }
    }
}

/// Get a unique ID for this thread. Internally mutates global counter.
/// TODO This should be mut somehow.
pub fn get_unique_det_id() -> u32 {
    DET_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

pub fn get_det_id() -> DetThreadId {
    DET_ID.with(|di| di.borrow().clone())
}

/// TODO Mark as mutable somewhere?
pub fn get_select_id() -> u32 {
    SELECT_ID.with(|id| *id.borrow())
}

pub fn inc_select_id() {
    SELECT_ID.with(|id| {
        *id.borrow_mut() += 1;
    });
}

thread_local! {
    /// TODO this id is probably redundant.
    static SELECT_ID: RefCell<u32> = RefCell::new(0);

    static DET_ID_SPAWNER: RefCell<DetIdSpawner> = RefCell::new(DetIdSpawner::new());
    /// Unique threadID assigned at thread spawn to to each thread.
    static DET_ID: RefCell<DetThreadId> = RefCell::new(DetThreadId::new());
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
    let new_id = DET_ID_SPAWNER.with(|spawner| {
        spawner.borrow_mut().new_child_det_id()
    });
    trace!("Assigned determinsitic id {:?} for new thread.", new_id);


    let new_spawner = DetIdSpawner::from(new_id.clone());

    thread::spawn(|| {
        // Initialize TLS for this thread.
        DET_ID.with(|id| {
            *id.borrow_mut() = new_id;
        });

        DET_ID_SPAWNER.with(|spawner| {
            *spawner.borrow_mut() = new_spawner;
        });

        // SELECT_ID is fine starting at 0.
        f()
    })
}

mod tests {
    #[test]
    /// TODO Add random delays to increase amount of nondeterminism.
    fn determinitic_ids() {
        use super::{spawn, get_det_id, DetThreadId};
        use std::thread::JoinHandle;

        let mut v1: Vec<JoinHandle<_>> = vec![];

        for i in 0..4 {
            let h1 = spawn(move || {
                let a = [i];
                assert_eq!(DetThreadId::from(&a[..]), get_det_id());
                // println!("({}): {:?}", i, get_det_id());

                let mut v2 = vec![];
                for j in 0..4 {
                    let h2 = spawn(move ||{
                        let a = [i, j];
                        assert_eq!(DetThreadId::from(&a[..]), get_det_id());
                        // println!("({}, {}): Det Thread Id: {:?}", i, j, get_det_id());
                    });
                    v2.push(h2);
                }
                for handle in v2 {
                    handle.join().unwrap();
                }
            });
            v1.push(h1);
        }
        for handle in v1 {
            handle.join().unwrap();
        }

        assert!(true);
    }
}
