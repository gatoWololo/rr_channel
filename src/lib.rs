#![feature(trait_alias)]
#![feature(const_type_name)]

use std::collections::{HashMap, VecDeque};
use std::env::var;
use std::env::VarError;

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use tracing::{debug, error, info, span, span::EnteredSpan, trace, warn, Level};

use crate::recordlog::{
    get_global_recorder, ChannelVariant, EVENT_NUMBER, EVENT_RECEIVER, EVENT_SENDER,
};
use desync::DesyncMode;
use detthread::DetThreadId;

// TODO Why is this being re-exported without `pub use`?
use crate::detthread::{get_det_id, THREAD_INITIALIZED};
use crate::error::DesyncError;
use crate::recordlog::{RecordEntry, RecordMetadata, TivoEvent};
use crate::rr::{DetChannelId, RecordEventChecker};
#[cfg(test)]
use crate::test::TEST_MODE;
use anyhow::Context;
use std::any::type_name;

pub mod crossbeam_channel;
mod crossbeam_select;
mod crossbeam_select_macro;
mod desync;
pub mod detthread;
mod error;
pub mod ipc_channel;
pub mod mpsc;
pub mod recordlog;
mod rr;
#[cfg(test)]
pub mod test;
mod utils;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RRMode {
    Record,
    Replay,
    NoRR,
}

/// To deterministically replay messages we pass our deterministic thread ID + the
/// original message.
pub type DetMessage<T> = (DetThreadId, T);

/// Every channel carries a buffer where message that shouldn't have arrived are stored.
pub type BufferedValues<T> = HashMap<DetThreadId, VecDeque<T>>;

const RECORD_MODE_VAR: &str = "RR_MODE";
const DESYNC_MODE_VAR: &str = "RR_DESYNC_MODE";
const RECORD_FILE_VAR: &str = "RR_RECORD_FILE";

fn get_rr_mode() -> RRMode {
    if cfg!(test) {
        #[cfg(test)]
        return *TEST_MODE
            .get()
            .expect("TEST_MODE one-cell not initialized.")
            .lock()
            .unwrap();
        unreachable!();
    } else {
        *RECORD_MODE.as_ref().expect("Cannot Get RR_MODE")
    }
}

///
pub(crate) fn get_rr_mode_checked() -> &'static anyhow::Result<RRMode> {
    &*RECORD_MODE
}

lazy_static! {
    /// Record type. Initialized from environment variable RR_CHANNEL.
    /// TODO Probably switch this to a one-cell with an init function to allow us to return
    /// errors once, without having to have the type be Result (I don't like having to match/expect
    /// everytime we access it, which happens a lot.
    static ref RECORD_MODE: anyhow::Result<RRMode> = {
        debug!("Initializing RECORD_MODE lazy static.");

        let value = var(RECORD_MODE_VAR).with_context(|| format!("Please specify RR mode via {}", RECORD_MODE_VAR))?;
        let mode = match value.to_ascii_lowercase().as_str() {
                    "record" => RRMode::Record,
                    "replay" => RRMode::Replay,
                    "norr"   => RRMode::NoRR,
                    e        => anyhow::bail!("Unknown record and replay mode: {}", e),
                };

        info!("Mode {:?} selected.", mode);
        Ok(mode)
    };

    /// Record type. Initialized from environment variable RR_CHANNEL.
    static ref DESYNC_MODE: DesyncMode = {
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
    static ref LOG_FILE_NAME: String = {
        debug!("Initializing RECORD_FILE lazy static.");

        match var(RECORD_FILE_VAR) {
            Ok(mode) => {
                info!("Mode {:?} selected.", mode);
                mode
            }
            Err(VarError::NotPresent) => {
                let s = "Unspecified record file. Please use env var RR_RECORD_FILE";
                warn!(s);
                panic!("{}", s);
            }
            Err(e @ VarError::NotUnicode(_)) => {
                let s = format!("RECORD_FILE value is not valid unicode: {}.", e);
                warn!("{}", s);
                panic!("{}", s);
            }
        }
    };
}

thread_local! {
    static MAIN_THREAD_SPAN: EnteredSpan = {
        let t = std::thread::current();

        span!(Level::INFO,
              "Thread",
              dti=?get_det_id(),
              name=t.name().unwrap_or("None")
              ).
            entered()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct EventRecorder {}

impl EventRecorder {
    fn get_global_recorder() -> EventRecorder {
        EventRecorder {}
    }

    fn next_record_entry(&self) -> Option<RecordEntry> {
        EVENT_NUMBER.with(|en| {
            let mut en = en.borrow_mut();
            *en += 1;
            warn!("EVENT_NUMBER: {}", *en);
        });

        EVENT_RECEIVER.with(|events| events.borrow_mut().next())
    }

    fn write_event_to_record(
        &self,
        event: TivoEvent,
        metadata: &RecordMetadata,
    ) -> desync::Result<()> {
        EVENT_NUMBER.with(|en| {
            let mut en = en.borrow_mut();
            *en += 1;
            warn!("EVENT_NUMBER: {}", *en);
        });

        let entry: RecordEntry = RecordEntry {
            event,
            channel_variant: metadata.channel_variant,
            chan_id: metadata.id.clone(),
            type_name: metadata.type_name.clone(),
        };

        debug!("About to recordlog entry: {:?}", entry);
        EVENT_SENDER
            .with(|event_sender| event_sender.send(entry))
            .map_err(|e| DesyncError::CannotWriteEventToLog(e))?;

        Ok(())
    }

    /// Get log entry returning the record, variant, and channel ID.
    fn get_log_entry(&self) -> desync::Result<RecordEntry> {
        self.next_record_entry().ok_or_else(|| {
            let error = DesyncError::NoEntryInLog;
            error!(%error);
            error
        })
    }
}

/// TODO: Implement Drop method for Tivo. Which checks that the `GLOBAL_REPLAYER` has been `take`en,
/// i.e. it holds None. This would mean the `execution_done` function has been called. The Drop
/// method should print a `tracing::warn`ing if it hasn't. This requires changing `GLOBAL_REPLAYER`
/// as we don't want the this destructor panicking... Is it worth it?
pub struct Tivo {}

impl Tivo {
    //
    pub fn init_tivo_thread_root_test() -> Tivo {
        info!("Initializing Thread Root.");
        Tivo::init_tivo_thread_root_test_do(false);
        Tivo {}
    }

    pub fn init_tivo_thread_root_with_router() -> Tivo {
        info!("Initializing Thread Root.");
        Tivo::init_tivo_thread_root_test_do(true);
        Tivo {}
    }

    fn init_tivo_thread_root_test_do(init_router: bool) {
        THREAD_INITIALIZED.with(|ti| {
            if *ti.borrow() {
                error!("Thread root already initialized!");
                panic!("Thread root already initialized!");
            }
            ti.replace(true);
        });

        // Init ROUTER from main thread. This guarantees a deterministic thread id for it. As threads
        // will not be racing to init it first.
        if init_router {
            crate::ipc_channel::router::ROUTER.init();
        }

        // Main thread span is explicitly initialized here. We only use this for the main thread
        // as it is easy enough to create a span for threads in the dethread::Builder::spawn
        // function. Also for some reason having threads live here causes a panic...
        MAIN_THREAD_SPAN.with(|_| {});
    }

    pub fn execution_done(self) -> anyhow::Result<()> {
        let _e = span!(Level::INFO, "RecordLogFlusher::drop").entered();

        let rr_mode = match get_rr_mode_checked() {
            Ok(mode) => mode,
            Err(e) => {
                let m = format!("RRMode not properly set. Reason: {:?}", e);
                error!("{}", m);
                anyhow::bail!(m);
            }
        };

        if *rr_mode == RRMode::Record {
            let mut gr = match get_global_recorder().lock() {
                Ok(gr) => gr,
                Err(e) => {
                    let m = format!("Cannot get GLOBAL_RECORDER lock. Reason {:?}", e);
                    error!("{}", m);
                    anyhow::bail!(m);
                }
            };

            match gr.take() {
                None => {
                    // Probably don't wanna panic in drop function...
                    // This shouldn't really happen, but let's still account for the case.
                    let m = "GLOBAL_RECORDER is None.";
                    error!("{}", m);
                    anyhow::bail!(m);
                }
                Some(gr) => {
                    if let Err(e) = gr.flush() {
                        let m = format!("Cannot write to file. Reason: {:?}", e);
                        error!("{}", m);
                        anyhow::bail!(m);
                    }
                }
            }
        }

        Ok(())
    }
}

struct ChannelCreation {}
impl RecordEventChecker<()> for ChannelCreation {
    fn check_recorded_event(&self, re: &TivoEvent) -> Result<(), TivoEvent> {
        match re {
            TivoEvent::ChannelCreation => Ok(()),
            _other => Err(TivoEvent::ChannelCreation),
        }
    }
}

pub(crate) fn rr_channel_creation_event<T>(
    mode: RRMode,
    id: &DetChannelId,
    recorder: &EventRecorder,
    variant: ChannelVariant,
) -> desync::Result<()> {
    let metadata = RecordMetadata::new(type_name::<T>().to_string(), variant, id.clone());
    match mode {
        RRMode::Record => {
            recorder.write_event_to_record(TivoEvent::ChannelCreation, &metadata)?;
        }
        RRMode::Replay => {
            let event = recorder.get_log_entry()?;
            // Can't propagate error up, panic.
            ChannelCreation {}.check_event_mismatch(&event, &metadata)?;
        }
        RRMode::NoRR => {}
    }
    Ok(())
}

// Uses closure to easily propagate error and handle it one location, makes code cleaner for
// functions that don't return desync::Result<()>
fn check_events(checker: impl Fn() -> desync::Result<()>) {
    if let Err(e) = checker() {
        error!(%e);
        panic!("{}", e);
    }
}
