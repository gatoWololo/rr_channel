#![feature(bind_by_move_pattern_guards)]
#![feature(core_intrinsics)]
#![feature(specialization)]

use env_logger;
use lazy_static::lazy_static;
use log::{debug, trace, warn};
use serde::{Deserialize, Serialize};

mod channel;
mod crossbeam_select;
pub mod ipc;
mod record_replay;
mod select;
pub mod thread;
pub mod router;

// Rexports.
pub use channel::{after, bounded, never, unbounded, Receiver, Sender};
pub use crossbeam_channel::RecvError;
pub use crossbeam_channel::RecvTimeoutError;
pub use crossbeam_channel::TryRecvError;
// pub use ipc::channel;
pub use record_replay::{LogEntry, RECORDED_INDICES, WRITE_LOG_FILE};
pub use select::{Select, SelectedOperation};
pub use thread::{current, panicking, park, park_timeout, sleep, yield_now};
pub use thread::{
    get_det_id, get_event_id, inc_event_id, DetIdSpawner, DetThreadId,
};

/// A singleton instance exists globally for the current mode via lazy_static global variable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RecordReplayMode {
    Record,
    Replay,
    NoRR,
}

const ENV_VAR_NAME: &str = "RR_CHANNEL";

lazy_static! {
    /// Singleton environment logger. Must be initialized somewhere, and only once.
    pub static ref ENV_LOGGER: () = {
        env_logger::init();
        // Init logger with no timestamp data or module name.
        // env_logger::Builder::from_default_env().
        //     default_format_timestamp(false).
        //     default_format_module_path(false).
        //     format(|buf, record| writeln!(buf, "({:?}, {:?}) {}",
        //                                   get_det_id_clone(),
        //                                   get_event_id(), record.args())).
        //     init();
    };
    /// Record type. Initialized from environment variable RR_CHANNEL.
    pub static ref RECORD_MODE: RecordReplayMode = {
        log_trace("Initializing RECORD_MODE lazy static.");
        use std::env::var;
        use std::env::VarError;
        let mode = match var(ENV_VAR_NAME) {
            Ok(value) => {
                match value.as_str() {
                    "record" => RecordReplayMode::Record,
                    "replay" => RecordReplayMode::Replay,
                    "noRR"   => RecordReplayMode::NoRR,
                    e        => {
                        warn!("Unkown record and replay mode: {}. Assuming noRR.", e);
                        RecordReplayMode::NoRR
                }
                }
            }
            Err(VarError::NotPresent) => RecordReplayMode::NoRR,
            Err(e @ VarError::NotUnicode(_)) => {
                warn!("RR_CHANNEL value is not valid unicode: {}, assuming noRR.", e);
                RecordReplayMode::NoRR
            }
        };

        log_trace(&format!("Mode {:?} selected.", mode));
        mode
    };
}

fn log_trace(msg: &str) {
    let thread = std::thread::current();
    trace!("name: {:?} | ({:?}, {:?}) | {}", thread.name(),
           get_det_id(), get_event_id(), msg);
}
