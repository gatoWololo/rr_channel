use log::Level::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::remove_file;
use std::fs::File;
use std::io::BufRead;
use std::sync::mpsc;
use std::sync::Mutex;

use crate::desync;
use crate::detthread::DetThreadId;
use crate::error;
use crate::rr::DetChannelId;
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

/// Interface necessary by all recorders. A recorder is any type that holds a record log for
/// tracking channel events.
pub(crate) trait Recordable {
    // TODO Maybe add return type to signify failure.
    // Ideally self should be a mutable reference because, it is. But this clashes with the
    // the channel library APIs which for some reason send and recv are not mutable. So to keep
    // things simple here we also say the self is not mutable :grimace-emoji:
    fn write_entry(&self, message: RecordEntry);

    fn write_event_to_record(&self, event: RecordedEvent, metadata: &RecordMetadata) {
        crate::log_rr!(Warn, "rr::log()");

        let entry: RecordEntry = RecordEntry {
            event,
            channel_variant: metadata.channel_variant,
            chan_id: metadata.id.clone(),
            type_name: metadata.type_name.clone(),
        };

        self.write_entry(entry.clone());

        crate::log_rr!(Warn, "Logged entry: {:?}", entry);
    }

    fn next_record_entry(&self) -> Option<RecordEntry>;

    /// Actual implementation. Compares `curr_flavor` and `curr_id` against
    /// log entry if they're Some(_).
    fn get_log_entry_do(
        &self,
        curr_flavor: Option<&ChannelVariant>,
        curr_id: Option<&DetChannelId>,
    ) -> desync::Result<(RecordedEvent, ChannelVariant, DetChannelId)> {
        match self.next_record_entry() {
            Some(record_entry) => {
                crate::log_rr!(Debug, "Event fetched: {:?}", record_entry);

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
            None => Err(error::DesyncError::NoEntryInLog),
        }
    }

    /// Get log entry and compare `curr_flavor` and `curr_id` with the values
    /// on the record.
    fn get_log_entry_with(
        &self,
        curr_flavor: &ChannelVariant,
        curr_id: &DetChannelId,
    ) -> desync::Result<RecordedEvent> {
        self.get_log_entry_do(Some(curr_flavor), Some(curr_id))
            .map(|(r, _, _)| r)
    }

    /// Get log entry with no comparison.
    fn get_log_entry(&self) -> desync::Result<RecordedEvent> {
        self.get_log_entry_do(None, None).map(|(r, _, _)| r)
    }

    /// Get log entry returning the record, flavor, and channel id if needed.
    fn get_log_entry_ret(&self) -> desync::Result<(RecordedEvent, ChannelVariant, DetChannelId)> {
        self.get_log_entry_do(None, None)
    }
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
