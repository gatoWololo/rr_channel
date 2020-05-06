use env_logger;
use lazy_static::lazy_static;
use log::warn;
use serde::{Deserialize, Serialize};

pub mod crossbeam;
mod crossbeam_select;
mod crossbeam_select_macro;
mod desync;
pub mod detthread;
mod error;
pub mod ipc;
pub mod mpsc;
mod recordlog;
pub mod router;
mod rr;

// pub use channel::{after, bounded, never, unbounded, Receiver, Sender};
// pub use crossbeam_channel::{RecvError, RecvTimeoutError, TryRecvError};
// pub use rr::{DetChannelId, LogEntry, RECORDED_INDICES, WRITE_LOG_FILE};
// pub use select::{Select, SelectedOperation};
// pub use thread::{
//     current, get_det_id, get_event_id, in_forwarding, inc_event_id, panicking, park, park_timeout,
//     sleep, yield_now, DetIdSpawner, DetThreadId,
// };

use desync::DesyncMode;
use log::Level::*;
use std::env::var;
use std::env::VarError;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RRMode {
    Record,
    Replay,
    NoRR,
}

const RECORD_MODE_VAR: &str = "RR_CHANNEL";
const DESYNC_MODE_VAR: &str = "RR_DESYNC_MODE";
const RECORD_FILE_VAR: &str = "RR_RECORD_FILE";

lazy_static! {
    /// Singleton environment logger. Must be initialized somewhere, and only once.
    pub static ref ENV_LOGGER: () = {
        env_logger::init();
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

/// Prints name of type based on <T> by reaching into compiler intrinsics. NIGHTLY ONLY.
fn get_generic_name<T>() -> &'static str {
    ""
    // "nightly-only"
    // Nightly only TODO: Set as optional Cargo.toml attribute?
    // unsafe { std::intrinsics::type_name::<T>() };
}
