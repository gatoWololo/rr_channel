use log::Level::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::remove_file;
use std::fs::File;
use std::io::BufRead;
use std::sync::mpsc;
use std::sync::Mutex;

use crate::detthread::DetThreadId;
use crate::error;
use crate::rr::DetChannelId;
use crate::{desync, detthread};
use crate::{RRMode, LOG_FILE_NAME, RECORD_MODE};

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
    /// Thread performing the operation's unique ID.
    /// (current_thread, event_id) form a unique key per entry in our map and log.
    pub current_thread: DetThreadId,
    /// Unique per-thread identifier given to every select dynamic instance.
    /// (current_thread, event_id) form a unique key per entry in our map and log.
    pub event_id: u32,
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
        current_thread: DetThreadId,
        event_id: u32,
        event: RecordedEvent,
        channel_variant: ChannelVariant,
        chan_id: DetChannelId,
        type_name: String,
    ) -> RecordEntry {
        RecordEntry {
            current_thread,
            event_id,
            event,
            channel_variant,
            chan_id,
            type_name,
        }
    }
}

/// Main enum listing different types of events that our logger supports. These recorded
/// events contain all information to replay the event. On errors, we can just return
/// the error that was witnessed during record. On successful read, the variant contains
/// the index or sender_thread_id where we should expect the message to arrive from.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum RecordedEvent {
    // Crossbeam select.
    SelectReady {
        select_index: usize,
    },
    Select(SelectEvent),
    // Crossbeam recv.
    RecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::RecvErrorDef")]
    RecvErr(crossbeam_channel::RecvError),
    // MPSC recv.
    MpscRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::MpscRecvErrorDef")]
    MpscRecvErr(mpsc::RecvError),
    // Crossbeam try_recv
    TryRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::TryRecvErrorDef")]
    TryRecvErr(crossbeam_channel::TryRecvError),
    // MPSC try_recv
    MpscTryRecvSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::MpscTryRecvErrorDef")]
    MpscTryRecvErr(mpsc::TryRecvError),
    // Crossbeam recv_timeout
    RecvTimeoutSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::RecvTimeoutErrorDef")]
    RecvTimeoutErr(crossbeam_channel::RecvTimeoutError),
    // MPSC recv_timeout
    MpscRecvTimeoutSucc {
        sender_thread: DetThreadId,
    },
    #[serde(with = "error::MpscRecvTimeoutErrorDef")]
    MpscRecvTimeoutErr(mpsc::RecvTimeoutError),
    // IPC recv
    IpcRecvSucc {
        sender_thread: DetThreadId,
    },
    IpcError(IpcErrorVariants),
    IpcTryRecvErrorEmpty,
    IpcTryRecvIpcError(IpcErrorVariants),
    // Crossbeam send.
    Sender,
    // MPSC (standard library) send
    MpscSender,
    // Ipc send.
    IpcSender,
    // IpcReceiverSet
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
        match *RECORD_MODE {
            RRMode::Record => {
                // Delete file if it already existed... file may not exist. That's okay.
                let _ = remove_file(LOG_FILE_NAME.as_str());
                let error = & format!("Unable to open {} for record logging.", *LOG_FILE_NAME);
                Mutex::new(File::create(LOG_FILE_NAME.as_str()).expect(error))
            }
            RRMode::Replay =>
                panic!("Write log file should not be accessed in replay mode."),
            RRMode::NoRR =>
                panic!("Write log file should not be accessed in no record-and-replay mode."),
        }
    };

    /// Global map holding all indexes from the record phase. Lazily initialized on replay
    /// mode.
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (RecordedEvent, ChannelVariant, DetChannelId)> =
     init_recorded_indices();
}

fn init_recorded_indices(
) -> HashMap<(DetThreadId, u32), (RecordedEvent, ChannelVariant, DetChannelId)> {
    crate::log_rr!(Debug, "Initializing RECORDED_INDICES lazy static.");
    use std::io::BufReader;

    let mut recorded_indices = HashMap::new();
    let log = File::open(LOG_FILE_NAME.as_str())
        .unwrap_or_else(|_| panic!("Unable to open {} for replay.", LOG_FILE_NAME.as_str()));
    let log = BufReader::new(log);

    for line in log.lines() {
        let line = line.expect("Unable to read recorded log file");
        let entry: RecordEntry = serde_json::from_str(&line).expect("Malformed log entry.");

        let curr_thread = entry.current_thread.clone();
        let key = (curr_thread, entry.event_id);
        let value = (
            entry.event.clone(),
            entry.channel_variant,
            entry.chan_id.clone(),
        );

        let prev = recorded_indices.insert(key, value);
        if prev.is_some() {
            panic!(
                "Corrupted log file. Adding key-value ({:?}, {:?}) but previous value \
                        {:?} already exited. Hashmap entries should be unique.",
                (entry.current_thread, entry.event_id),
                (entry.event, entry.channel_variant, entry.chan_id),
                prev
            );
        }
    }

    recorded_indices
}

/// Interface necessary by all recorders. A recorder is any type that holds a record log for
/// tracking channel events.
pub(crate) trait Recordable {
    // TODO Maybe add return type to signify failure.
    // Ideally self should be a mutable reference because, it is. But this clashes with the
    // the channel library APIs which for some reason send and recv are not mutable. So to keep
    // things simple here we also say the self is not mutable :grimace-emoji:
    fn write_entry(&self, id: DetThreadId, event: EventId, message: RecordEntry);

    fn write_event_to_record(&self, event: RecordedEvent, metadata: &RecordMetadata) {
        crate::log_rr!(Warn, "rr::log()");

        // Write our (DET_ID, EVENT_ID) -> index to our log file
        let current_thread = detthread::get_det_id();
        let event_id = detthread::get_event_id();

        let entry: RecordEntry = RecordEntry {
            current_thread: current_thread.clone(),
            event_id,
            event,
            channel_variant: metadata.channel_variant,
            chan_id: metadata.id.clone(),
            type_name: metadata.type_name.clone(),
        };

        self.write_entry(current_thread, event_id, entry.clone());

        crate::log_rr!(
            Warn,
            "{:?} Logged entry: {:?}",
            entry,
            (detthread::get_det_id(), event_id)
        );

        // TODO Why does writing to the log increment the event id?
        detthread::inc_event_id();
    }

    fn get_record_entry(&self, key: &(DetThreadId, EventId)) -> Option<RecordEntry>;

    /// Actual implementation. Compares `curr_flavor` and `curr_id` against
    /// log entry if they're Some(_).
    fn get_log_entry_do(
        &self,
        our_thread: DetThreadId,
        event_id: EventId,
        curr_flavor: Option<&ChannelVariant>,
        curr_id: Option<&DetChannelId>,
    ) -> desync::Result<(RecordedEvent, ChannelVariant, DetChannelId)> {
        let key = (our_thread, event_id);

        match self.get_record_entry(&key) {
            Some(record_entry) => {
                crate::log_rr!(
                    Debug,
                    "Event fetched: {:?} for keys: {:?}",
                    record_entry,
                    key
                );

                if let Some(curr_flavor) = curr_flavor {
                    if record_entry.channel_variant != *curr_flavor {
                        return Err(error::DesyncError::ChannelVariantMismatch(
                            record_entry.channel_variant,
                            *curr_flavor,
                        ));
                    }
                }
                if let Some(curr_id) = curr_id {
                    if record_entry.chan_id != *curr_id {
                        return Err(error::DesyncError::ChannelMismatch(
                            record_entry.chan_id,
                            curr_id.clone(),
                        ));
                    }
                }
                Ok((
                    record_entry.event,
                    record_entry.channel_variant,
                    record_entry.chan_id,
                ))
            }
            None => Err(error::DesyncError::NoEntryInLog(key.0, key.1)),
        }
    }

    /// Get log entry and compare `curr_flavor` and `curr_id` with the values
    /// on the record.
    fn get_log_entry_with(
        &self,
        our_thread: DetThreadId,
        event_id: EventId,
        curr_flavor: &ChannelVariant,
        curr_id: &DetChannelId,
    ) -> desync::Result<RecordedEvent> {
        self.get_log_entry_do(our_thread, event_id, Some(curr_flavor), Some(curr_id))
            .map(|(r, _, _)| r)
    }

    /// Get log entry with no comparison.
    fn get_log_entry(
        &self,
        our_thread: DetThreadId,
        event_id: EventId,
    ) -> desync::Result<RecordedEvent> {
        self.get_log_entry_do(our_thread, event_id, None, None)
            .map(|(r, _, _)| r)
    }

    /// Get log entry returning the record, flavor, and channel id if needed.
    fn get_log_entry_ret(
        &self,
        our_thread: DetThreadId,
        event_id: EventId,
    ) -> desync::Result<(RecordedEvent, ChannelVariant, DetChannelId)> {
        self.get_log_entry_do(our_thread, event_id, None, None)
    }
}

pub type EventId = u32;

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
    pub fn new(
        type_name: String,
        flavor: ChannelVariant,
        id: DetChannelId,
    ) -> RecordMetadata {
        RecordMetadata {
            type_name,
            channel_variant: flavor,
            id,
        }
    }
}
