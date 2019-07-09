#![feature(bind_by_move_pattern_guards)]
#![feature(core_intrinsics)]
#![feature(specialization)]

use lazy_static::lazy_static;
use env_logger;
use log::{trace, debug, warn};
use std::io::Write;

mod channels;
mod select;
mod crossbeam_select;
pub mod thread;
mod record_replay;

// Rexports.
pub use thread::{current, yield_now, sleep, panicking, park, park_timeout};
pub use record_replay::{LogEntry, WRITE_LOG_FILE, RECORDED_INDICES};
pub use channels::{unbounded, bounded, Sender, Receiver, after, never};
pub use thread::{DetIdSpawner, DetThreadId, get_det_id, get_select_id,
                 inc_select_id};
pub use select::{Select, SelectedOperation};
pub use crossbeam_channel::RecvTimeoutError;
pub use crossbeam_channel::TryRecvError;
pub use crossbeam_channel::RecvError;

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
            format(|buf, record| writeln!(buf, "({:?}, {:?}) {}", get_det_id(),
                                          get_select_id(), record.args())).
            init();
    };
    /// Record type. Initialized from environment variable RR_CHANNEL.
    pub static ref RECORD_MODE: RecordReplayMode = {
        trace!("Initializing RECORD_MODE lazy static.");
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

        trace!("Mode {:?} selected.", mode);
        mode
    };
}
