#![feature(bind_by_move_pattern_guards)]

use lazy_static::lazy_static;
use env_logger;
use log::{trace, debug};

mod channels;
mod crossbeam_select;
mod det_id;
mod record_replay;

pub use record_replay::{LogEntry, WRITE_LOG_FILE, RECORDED_INDICES};
pub use channels::unbounded;
pub use det_id::{DetIdSpawner, DetThreadId, get_det_id, get_select_id,
                 inc_select_id, spawn};

/// A singleton instance exists globally for the current mode via lazy_static global variable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RecordReplayMode {
    Record,
    Replay,
    NoRR,
}

const ENV_VAR_NAME: &str = "RR_CHANNEL";

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
                          make_replay, inc_select_id, LogEntry, DetThreadId};
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


