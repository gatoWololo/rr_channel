use std::collections::hash_map::Entry;
use std::collections::vec_deque::IntoIter;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use tracing::{debug, info, span, span::EnteredSpan, trace, warn, Level};

use crate::detthread::DetThreadId;
use crate::error;
use crate::error::DesyncError;
use crate::fn_basename;
use crate::get_det_id;
use crate::rr::DetChannelId;
use crate::LOG_FILE_NAME;
use anyhow::Context;
use std::cell::RefCell;

/// Record representing a successful select from a channel. Used in replay mode.
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub enum SelectEvent {
    Success {
        /// For multiple producer channels we need to diffentiate who the sender was.
        /// As the index only tells us the correct receiver end, but not who the sender was.
        sender_thread: DetThreadId,
        /// Index of selected channel for select!()
        selected_index: usize,
    },
    RecvError {
        /// Index of selected channel for select!(). We still need this even onf RecvError
        /// as user might call .index() method on SelectedOperation and we need to be able
        /// to return the real index.
        selected_index: usize,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub enum IpcSelectEvent {
    MessageReceived(/*index:*/ u64, DetThreadId),
    ChannelClosed(/*index:*/ u64),
}

/// We tag our record-log entries with the type of channel they represent. This lets us compares
/// their channel-type to ensure it is what we expect between record and replay. Just one more
/// sanity check for robustness.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Copy, Clone)]
pub enum ChannelVariant {
    MpscBounded,
    MpscUnbounded,
    CbUnbounded,
    CbAfter,
    CbBounded,
    CbNever,
    None,
    Ipc,
    IpcSelect,
    Select,
}

/// Represents a single entry in our log. Technically all that is needed is the current_thread_id,
/// event_id, and event. Everything else is metadata used for checks to ensure we're reading from
/// the right channel.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RecordEntry {
    /// Actual event and all information needed to replay this event.
    pub event: TivoEvent,
    // TODO Replace this field with RecordMetadata type.
    pub channel_variant: ChannelVariant,
    /// Identifier of the channel who this entry was recorded for.
    pub chan_id: DetChannelId,
    pub type_name: String,
}

impl RecordEntry {
    pub(crate) fn new(
        event: TivoEvent,
        channel_variant: ChannelVariant,
        chan_id: DetChannelId,
        type_name: String,
    ) -> RecordEntry {
        RecordEntry {
            event,
            channel_variant,
            chan_id,
            type_name,
        }
    }

    pub(crate) fn check_mismatch(&self, metadata: &RecordMetadata) -> Result<(), DesyncError> {
        if self.channel_variant != metadata.channel_variant {
            let error =
                DesyncError::ChannelVariantMismatch(self.channel_variant, metadata.channel_variant);
            error!("{}", error);
            return Err(error);
        }

        if self.chan_id != metadata.id {
            let error = DesyncError::ChannelMismatch(self.chan_id.clone(), metadata.id.clone());
            error!("{}", error);
            return Err(error);
        }

        // Checking types is redundant if channel IDs are the same types should be the same.
        // TODO This is checked during channel creation events.
        // if self.type_name != metadata.type_name

        Ok(())
    }
}

/// Main enum listing different types of events that our logger supports. These recorded
/// events contain all information to replay the event. On errors, we can just return
/// the error that was witnessed during record. On successful read, the variant contains
/// the index or sender_thread_id where we should expect the message to arrive from.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TivoEvent {
    // Crossbeam events.
    CrossbeamSender,
    CrossbeamRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::RecvErrorDef")]
    CrossbeamRecvErr(crossbeam_channel::RecvError),
    CrossbeamSelectReady {
        select_index: usize,
    },
    CrossbeamSelect(SelectEvent),

    CrossbeamRecvTimeoutSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::RecvTimeoutErrorDef")]
    CrossbeamRecvTimeoutErr(crossbeam_channel::RecvTimeoutError),

    CrossbeamTryRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::TryRecvErrorDef")]
    CrossbeamTryRecvErr(crossbeam_channel::TryRecvError),

    // std MPSC events.
    MpscRecvTimeoutSucc {
        sender_thread: DetThreadId,
    },
    MpscRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::MpscRecvErrorDef")]
    MpscRecvErr(mpsc::RecvError),
    MpscSender,
    MpscTryRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::MpscTryRecvErrorDef")]
    MpscTryRecvErr(mpsc::TryRecvError),
    #[serde(with = "error::MpscRecvTimeoutErrorDef")]
    MpscRecvTimeoutErr(mpsc::RecvTimeoutError),

    // ipc-channel events.
    IpcRecvSucc {
        sender_thread: DetThreadId,
    },
    IpcError(IpcErrorVariants),
    IpcTryRecvErrorEmpty,
    IpcTryRecvIpcError(IpcErrorVariants),
    IpcSender,
    IpcSelectAdd(/*index:*/ u64),
    IpcSelect {
        select_events: Vec<IpcSelectEvent>,
    },
    /// Current thread spawned new child thread with this DTI.
    ThreadInitialized(DetThreadId),
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub enum IpcErrorVariants {
    Bincode,
    Io,
    Disconnected,
}

pub(crate) fn get_global_recorder() -> &'static Mutex<Option<GlobalRecorder>> {
    &GLOBAL_RECORDER
}

lazy_static::lazy_static! {
    static ref GLOBAL_RECORDER: Mutex<Option<GlobalRecorder>> = {
        if cfg!(test) {
            return GlobalRecorder::init_no_file();
        } else {
            GlobalRecorder::init(&LOG_FILE_NAME)
        }
    };

    static ref GLOBAL_REPLAYER: Mutex<GlobalReplayer> = {
        if cfg!(test) {
            let mut gr = GLOBAL_RECORDER.lock().unwrap();
            if gr.is_none() {
                panic!("GLOBAL Recorder already taken.");
            }
            let entries = gr.take().unwrap().get_all_entries();
            Mutex::new(GlobalReplayer::init_from(entries))
        } else {
            Mutex::new(GlobalReplayer::new(&LOG_FILE_NAME))
        }
    };
}

thread_local! {
    /// We have several requirements for our logger. Mainly:
    /// 1) Child threads can die at any time. We cannot have their information lost if they exit
    ///    before we have a time to flush their values to a file.
    /// 2) We want to avoid having to use Mutex and Refcells for the recordlog, which are necessary
    ///    as the recordlog must live somewhere globally. Either thread local or true global state.
    /// 3) The main thread will be responsible for writing results to a file.
    ///
    /// Therefore we use channels which are implemented as lock-free data structures. So the logs
    /// of threads live somewhere shared where the main thread always has access to it.
    /// TODO Technically there is a data race here. If a thread spawns and tries to init
    ///      EVENT_SENDER once the GLOBAL_RECORDER has already been flushed. It will fail since
    ///      GLOBAL_RECORDER will be None.
    /// TODO We probably wanna implement a destructor which flushes the GLOBAL_RECORDER in case of
    ///      panic. Having even the partial recordlog would be useful. I'm tired of implementing
    ///      this so I will not do it now. To implement this we must make sure the flushing function
    ///      cannot panic, write now, it can. This mostly entails changing some of our lazy_static
    ///      globals so they become anyhow::Result<T> if they could not be inited properly.
    pub(crate) static EVENT_SENDER: crossbeam_channel::Sender<RecordEntry> = {
        let (s, r) = crossbeam_channel::unbounded();
        let mut gr = GLOBAL_RECORDER.lock().expect("Unable to acquire GLOBAL_RECORDER lock.");

        let e = "Cannot acquire mutable reference to GLOBAL_RECORDER. It has been taken.";
        let global_recorder = gr.as_mut().expect(e);
        global_recorder.add_receiver(get_det_id(), r);
        s
    };

    pub(crate) static EVENT_RECEIVER: RefCell<IntoIter<RecordEntry>> = {
        let mut gr = GLOBAL_REPLAYER.lock().unwrap();
        RefCell::new(gr.take_entry(&get_det_id()))
    };

    /// Keeps track of what event number we are per thread. This information is useful to know what
    /// event we are currently on when looking at our tracing output. This is updated as events are
    /// fetched or written to for record/replay respectively.
    pub(crate) static EVENT_NUMBER: RefCell<u32> = RefCell::new(0);
}

/// TODO It seems there is repetition in the fields here and on LogEntry?
/// Do we really need both?
#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct RecordMetadata {
    /// Owned for EZ serialize, deserialize.
    pub(crate) type_name: String,
    pub(crate) channel_variant: ChannelVariant,
    /// Unique identifier assigned to every channel. Deterministic and unique even with
    /// racing thread creation. DetThreadId refers to the original creator of this thread.
    /// The partner Receiver and Sender shares the same id.
    pub(crate) id: DetChannelId,
}

impl RecordMetadata {
    pub fn new(type_name: String, flavor: ChannelVariant, id: DetChannelId) -> RecordMetadata {
        RecordMetadata {
            type_name,
            channel_variant: flavor,
            id,
        }
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct GlobalReplayer {
    replayer: HashMap<DetThreadId, VecDeque<RecordEntry>>,
}

impl GlobalReplayer {
    pub fn new(path: &str) -> GlobalReplayer {
        let _s = Self::span(fn_basename!());
        info!("Initialized from file {:?}", path);

        let input = std::fs::read_to_string(&path).unwrap();

        let replayer: HashMap<DetThreadId, VecDeque<RecordEntry>> = ron::from_str(&input).unwrap();

        GlobalReplayer { replayer }
    }

    fn span(fn_name: &str) -> EnteredSpan {
        span!(Level::INFO, stringify!(GlobalReplayer), fn_name).entered()
    }

    pub(crate) fn init_from(
        replayer: HashMap<DetThreadId, VecDeque<RecordEntry>>,
    ) -> GlobalReplayer {
        if cfg!(not(test)) {
            panic!("Test-only code somehow ran in non-test environment");
        }

        let _s = Self::span(fn_basename!());
        info!(
            "Initialized from manual replayer with keys: {:?}.",
            replayer.keys()
        );
        GlobalReplayer { replayer }
    }

    fn take_entry(&mut self, det_id: &DetThreadId) -> IntoIter<RecordEntry> {
        let _s = Self::span(fn_basename!());

        match self.replayer.remove(&det_id) {
            None => {
                let e = format!("GlobalReplayer has no entry for {:?}", det_id,);
                error!(%e);
                panic!("{}", e);
            }
            Some(v) => {
                info!("Entry for {:?} taken by thread.", det_id);
                v.into_iter()
            }
        }
    }
}

#[cfg(test)]
/// Initialize the GLOBAL_RECORDER. This way, when the GLOBAL_REPLAYER is lazily initialized, it
/// will take the values from the GLOBAL_RECORDER. Effectively initializing the GLOBAL_REPLAYER.
pub(crate) fn set_global_memory_replayer(replayer: HashMap<DetThreadId, VecDeque<RecordEntry>>) {
    let _s = span!(Level::INFO, stringify!(set_global_memory_recorder)).entered();
    let mut hm: HashMap<DetThreadId, crossbeam_channel::Receiver<RecordEntry>> = HashMap::new();

    for (det_id, entries) in replayer {
        let (s, r) = crossbeam_channel::unbounded();
        for e in entries {
            s.send(e).expect("Send failed? How?!");
        }

        hm.insert(det_id, r);
    }

    let mut gr = GLOBAL_RECORDER.lock().unwrap();
    *gr = Some(GlobalRecorder {
        recorder: hm,
        path: None,
    });
}

pub(crate) fn take_global_memory_recorder() -> HashMap<DetThreadId, VecDeque<RecordEntry>> {
    let gr = GLOBAL_RECORDER.lock().unwrap().take().unwrap();
    gr.get_all_entries()
}

/// Threads can be killed at any time we must make sure we flush the GlobalRecorder to a file
/// without
pub(crate) struct GlobalRecorder {
    recorder: HashMap<DetThreadId, crossbeam_channel::Receiver<RecordEntry>>,
    path: Option<String>,
}

impl GlobalRecorder {
    pub(crate) fn init_no_file() -> Mutex<Option<GlobalRecorder>> {
        if cfg!(not(test)) {
            panic!("Test-only code somehow ran in non-test environment");
        }

        Mutex::new(Some(GlobalRecorder {
            recorder: HashMap::new(),
            path: None,
        }))
    }

    pub(crate) fn init(path: &str) -> Mutex<Option<GlobalRecorder>> {
        Mutex::new(Some(GlobalRecorder {
            recorder: HashMap::new(),
            path: Some(path.to_string()),
        }))
    }

    fn add_receiver(
        &mut self,
        sender_id: DetThreadId,
        r: crossbeam_channel::Receiver<RecordEntry>,
    ) {
        match self.recorder.entry(sender_id) {
            Entry::Occupied(o) => {
                let e = format!("Entry already present for {:?} in GlobalRecorder.", o.key());
                error!(%e);
                panic!("{}", e);
            }
            Entry::Vacant(v) => {
                v.insert(r);
            }
        }
    }

    fn get_all_entries(self) -> HashMap<DetThreadId, VecDeque<RecordEntry>> {
        let mut hm: HashMap<DetThreadId, VecDeque<RecordEntry>> = HashMap::new();
        for (id, event_receiver) in self.recorder {
            let events: VecDeque<_> = event_receiver.try_iter().collect();
            if hm.insert(id, events).is_some() {
                unreachable!(
                    "Our GlobalRecorder invariants guarantee we will never have \
                     duplicates here."
                );
            }
        }
        hm
    }

    // Write to file.
    pub(crate) fn flush(self) -> anyhow::Result<()> {
        use anyhow::bail;

        info!("Flushing Global recordlog.");

        if self.path.is_none() {
            bail!("Flush called in record mode?");
        }
        let path = self
            .path
            .clone()
            .context("Unable to get path. GlobalRecorder not init properly?")?;
        let hm = self.get_all_entries();

        if hm.len() == 0 {
            bail!("No entries in recordlog. Perhaps you dropped the value returned from init_tivo_thread_root_test too early?");
        }

        let serialized =
            ron::to_string(&hm).with_context(|| "Unable to serialize our data structure.")?;
        std::fs::write(&path, serialized)
            .with_context(|| format!("Unable to write recordlog to file: {:?}", path))?;
        Ok(())
    }
}
