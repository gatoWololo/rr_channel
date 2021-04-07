use std::collections::{HashMap, VecDeque};
use std::fs::remove_file;
use std::fs::File;
use std::sync::mpsc;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use tracing::{debug, info, span, span::EnteredSpan, trace, warn, Level};

use crate::detthread::DetThreadId;
use crate::error;
use crate::error::DesyncError;
use crate::rr::DetChannelId;
use crate::LOG_FILE_NAME;

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
pub(crate) struct RecordEntry {
    /// Actual event and all information needed to replay this event.
    pub event: RecordedEvent,
    // TODO Replace this field with RecordMetadata type.
    pub channel_variant: ChannelVariant,
    /// Identifier of the channel who this entry was recorded for.
    pub chan_id: DetChannelId,
    pub type_name: String,
}

impl RecordEntry {
    pub(crate) fn new(
        event: RecordedEvent,
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

        Ok(())
    }
}

/// Main enum listing different types of events that our logger supports. These recorded
/// events contain all information to replay the event. On errors, we can just return
/// the error that was witnessed during record. On successful read, the variant contains
/// the index or sender_thread_id where we should expect the message to arrive from.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum RecordedEvent {
    CbSelectReady {
        select_index: usize,
    },
    CbSelect(SelectEvent),
    CbRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::RecvErrorDef")]
    CbRecvErr(crossbeam_channel::RecvError),
    MpscRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::MpscRecvErrorDef")]
    MpscRecvErr(mpsc::RecvError),
    CbTryRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::TryRecvErrorDef")]
    CbTryRecvErr(crossbeam_channel::TryRecvError),
    MpscTryRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::MpscTryRecvErrorDef")]
    MpscTryRecvErr(mpsc::TryRecvError),
    CbRecvTimeoutSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::RecvTimeoutErrorDef")]
    CbRecvTimeoutErr(crossbeam_channel::RecvTimeoutError),
    MpscRecvTimeoutSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::MpscRecvTimeoutErrorDef")]
    MpscRecvTimeoutErr(mpsc::RecvTimeoutError),
    IpcRecvSucc {
        sender_thread: DetThreadId,
    },
    IpcError(IpcErrorVariants),
    IpcTryRecvErrorEmpty,
    IpcTryRecvIpcError(IpcErrorVariants),
    CbSender,
    MpscSender,
    IpcSender,
    IpcSelectAdd(/*index:*/ u64),
    IpcSelect {
        select_events: Vec<IpcSelectEvent>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub enum IpcErrorVariants {
    Bincode,
    Io,
    Disconnected,
}

lazy_static::lazy_static! {
    /// Global log file which all threads write to.
    pub static ref WRITE_LOG_FILE: Mutex<File> = {
        // Delete file if it already existed... file may not exist. That's okay.
        let _ = remove_file(LOG_FILE_NAME.as_str());
        let error = & format!("Unable to open {} for record logging.", *LOG_FILE_NAME);
        Mutex::new(File::create(LOG_FILE_NAME.as_str()).expect(error))
    };

    /// Global map holding all indexes from the record phase. Lazily initialized on replay
    /// mode.
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (RecordedEvent, ChannelVariant, DetChannelId)> =
     init_recorded_indices();
}

fn init_recorded_indices(
) -> HashMap<(DetThreadId, u32), (RecordedEvent, ChannelVariant, DetChannelId)> {
    todo!()
    // crate::log_rr!(Debug, "Initializing RECORDED_INDICES lazy static.");
    // use std::io::BufReader;
    //
    // let mut recorded_indices = HashMap::new();
    // let log = File::open(LOG_FILE_NAME.as_str())
    //     .unwrap_or_else(|_| panic!("Unable to open {} for replay.", LOG_FILE_NAME.as_str()));
    // let log = BufReader::new(log);
    //
    // for line in log.lines() {
    //     let line = line.expect("Unable to read recorded log file");
    //     let entry: RecordEntry = serde_json::from_str(&line).expect("Malformed log entry.");
    //
    //     // let curr_thread = entry.current_thread.clone();
    //     let key = (curr_thread, todo!());
    //     let value = (
    //         entry.event.clone(),
    //         entry.channel_variant,
    //         entry.chan_id.clone(),
    //     );
    //
    //     let prev = recorded_indices.insert(key, value);
    //     if prev.is_some() {
    //         panic!(
    //             "Corrupted log file. Adding key-value ({:?}, {:?}) but previous value \
    //                     {:?} already exited. Hashmap entries should be unique.",
    //             (todo!(), todo!()),
    //             (entry.event, entry.channel_variant, entry.chan_id),
    //             prev
    //         );
    //     }
    // }
    //
    // recorded_indices
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

/// Record and Replay using a memory-based recorder. Useful for unit testing channel methods.
/// These derives are needed because IpcReceiver requires Ser/Des... sigh...
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub(crate) struct InMemoryRecorder {
    queue: VecDeque<RecordEntry>,
    /// The ID is the same for all events as this is a thread local record.
    /// TODO: this field isn't used... remove it?
    dti: DetThreadId,
}

impl InMemoryRecorder {
    pub fn new(dti: DetThreadId) -> InMemoryRecorder {
        InMemoryRecorder {
            dti,
            queue: VecDeque::new(),
        }
    }

    pub(crate) fn add_entry(&mut self, r: RecordEntry) {
        self.queue.push_back(r);
    }

    pub(crate) fn get_next_entry(&mut self) -> Option<RecordEntry> {
        self.queue.pop_front()
    }
}
