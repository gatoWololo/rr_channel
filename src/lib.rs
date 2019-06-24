#![feature(bind_by_move_pattern_guards)]

use lazy_static::lazy_static;
use env_logger;
use log::{trace, debug};

mod channels;
mod select;
mod crossbeam_select;
mod det_id;
mod record_replay;

pub use record_replay::{LogEntry, WRITE_LOG_FILE, RECORDED_INDICES};
pub use channels::{unbounded, Sender, Receiver};
pub use det_id::{DetIdSpawner, DetThreadId, get_det_id, get_select_id,
                 inc_select_id, spawn};
pub use select::{Select, SelectedOperation};

/// A singleton instance exists globally for the current mode via lazy_static global variable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
