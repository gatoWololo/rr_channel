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
pub mod router;
mod select;
pub mod thread;

// Rexports.
pub use channel::{after, bounded, never, unbounded, Receiver, Sender};
pub use crossbeam_channel::RecvError;
pub use crossbeam_channel::RecvTimeoutError;
pub use crossbeam_channel::TryRecvError;
pub use record_replay::{LogEntry, RECORDED_INDICES, WRITE_LOG_FILE};
pub use select::{Select, SelectedOperation};
use thread::in_forwarding;
pub use thread::{current, panicking, park, park_timeout, sleep, yield_now};
pub use thread::{get_det_id, get_event_id, inc_event_id, DetIdSpawner, DetThreadId};

use crate::record_replay::DetChannelId;

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
    let event_name = if in_forwarding() {
        "ROUTER".to_string()
    } else {
        format!("{:?}", (get_det_id(), get_event_id()))
    };

    trace!(
        "thread: {:?} | event# {:?} {} | {}",
        thread.name(),
        event_name,
        get_event_id(),
        msg
    );
}

fn log_trace_with(msg: &str, id: &DetChannelId) {
    let thread = std::thread::current();
    let event_name = if in_forwarding() {
        "ROUTER".to_string()
    } else {
        format!("{:?}", (get_det_id(), get_event_id()))
    };

    trace!(
        "thread: {:?} | event# {:?} | {} | chan: {:?}",
        thread.name(),
        event_name,
        msg,
        id
    );
}
