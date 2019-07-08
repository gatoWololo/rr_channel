use lazy_static::lazy_static;
use log::trace;
use crate::RecordReplayMode;
use std::sync::Mutex;
use crate::DetThreadId;
use std::fs::File;
use crate::RECORD_MODE;
use std::fs::remove_file;

use std::collections::HashMap;
use std::io::BufRead;
use log::debug;
use crate::thread::get_det_id;
use crate::thread::get_select_id;
use crate::thread::inc_select_id;
use serde::{Serialize, Deserialize};

// TODO use environment variables to generalize this.
const LOG_FILE_NAME: &str = "/home/gatowololo/det_file.txt";

/// Record representing a sucessful select from a channel. Used in replay mode.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SelectEvent {
    Success {
        /// For multiple producer channels we need to diffentiate who the sender was.
        /// As the index only tells us the correct receiver end, but not who the sender was.
        sender_thread: DetThreadId,
        /// Index of selected channel for select!()
        selected_index: usize
    },
    RecvError {
        /// Index of selected channel for select!(). We still need this even onf RecvError
        /// as user might call .index() method on SelectedOperation and we need to be able
        /// to return the real index.
        selected_index: usize
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ReceiveEvent {
    Success {
        /// For multiple producer channels we need to diffentiate who the sender was.
        /// As the index only tells us the correct receiver end, but not who the sender was.
        sender_thread: DetThreadId,
    },
    RecvError,
}

/// Record representing results of calling Receiver::try_recv()
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TryRecvEvent {
    Success {
        /// For multiple producer channels we need to diffentiate who the sender was.
        /// As the index only tells us the correct receiver end, but not who the sender was.
        sender_thread: DetThreadId,
    },
    Disconnected,
    Empty
}

/// Record representing results of calling Receiver::recv_timeout()
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RecvTimeoutEvent {
    Success {
        /// For multiple producer channels we need to diffentiate who the sender was.
        /// As the index only tells us the correct receiver end, but not who the sender was.
        sender_thread: DetThreadId,
    },
    Disconnected,
    Timedout
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Copy, Clone)]
pub enum FlavorMarker {
    Unbounded,
    After,
    Bounded,
    Never,
    None,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LogEntry {
    /// Thread performing the operation's unique ID.
    /// (current_thread, select_id) form a unique key per entry in our map and log.
    pub current_thread: DetThreadId,
    /// Unique per-thread identifier given to every select dynamic instance.
    /// (current_thread, select_id) form a unique key per entry in our map and log.
    pub select_id: u32,
    pub event: RecordedEvent,
    pub channel: FlavorMarker,
    // pub real_thread_id: String,
    // pub pid: u32,
    pub type_name: String
}



#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RecordedEvent {
    SelectReady {
        select_index: usize,
    },
    Select(SelectEvent),
    Receive(ReceiveEvent),
    TryRecv(TryRecvEvent),
    RecvTimeout(RecvTimeoutEvent)
}

lazy_static! {
    /// Global log file which all threads write to.
    pub static ref WRITE_LOG_FILE: Mutex<File> = {
        trace!("Initializing WRITE_LOG_FILE lazy static.");

        match *RECORD_MODE {
            RecordReplayMode::Record => {
                // Delete file if it already existed... file may not exist. That's okay.
                let _ = remove_file(LOG_FILE_NAME);
                let error = & format!("Unable to open {} for record logging.", LOG_FILE_NAME);
                Mutex::new(File::create(LOG_FILE_NAME).expect(error))
            }
            RecordReplayMode::Replay =>
                panic!("Write log file should not be accessed in replay mode."),
            RecordReplayMode::NoRR =>
                panic!("Write log file should not be accessed in no record-and-replay mode."),
        }
    };

    /// Global map holding all indexes from the record phase.
    /// Lazily initialized on replay mode.
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (RecordedEvent, FlavorMarker)> = {
        trace!("Initializing RECORDED_INDICES lazy static.");
        use std::io::BufReader;

        let mut recorded_indices = HashMap::new();
        let log = File::open(LOG_FILE_NAME).
            expect(& format!("Unable to open {} for replay.", LOG_FILE_NAME));
        let log = BufReader::new(log);

        for line in log.lines() {
            let line = line.expect("Unable to read recorded log file");
            let entry: LogEntry = serde_json::from_str(&line).expect("Malformed log entry.");

            let key = (entry.current_thread.clone(), entry.select_id);
            let value = (entry.event.clone(), entry.channel);

            let prev = recorded_indices.insert(key, value);
            if prev.is_some() {
                panic!("Failed to replay. Adding key-value ({:?}, {:?}) but previous value \
                        {:?} already exited. Hashmap entries should be unique.",
                       (entry.current_thread, entry.select_id),
                       (entry.event, entry.channel),
                       prev);
            }

        }

        trace!("{:?}", recorded_indices);
        recorded_indices
    };
}

/// Unwrap rr_channels return value from calling .recv() and log results. Should only
/// be called in Record Mode.
pub fn log(event: RecordedEvent, channel: FlavorMarker, type_name: &str) {
    use std::io::Write;

    // Write our (DET_ID, SELECT_ID) -> index to our log file
    let current_thread = get_det_id();
    let select_id = get_select_id();
    let tid = format!("{:?}", ::std::thread::current().id());
    let pid = std::process::id();
    let entry: LogEntry = LogEntry { current_thread, select_id, event, channel,
                                     // real_thread_id: tid, pid,
                                     type_name: type_name.to_owned() };
    let serialized = serde_json::to_string(&entry).unwrap();

    WRITE_LOG_FILE.
        lock().
        unwrap().
        write_fmt(format_args!("{}\n", serialized)).
        expect("Unable to write to log file.");

    debug!("Logged entry: {:?}", entry);
    inc_select_id();
}

pub fn get_log_entry<'a>(our_thread: DetThreadId, select_id: u32)
                     -> (&'a RecordedEvent, FlavorMarker) {
    trace!("Replaying for our_thread: {:?}, select_id {:?}",
           our_thread, select_id);

    let key = (our_thread, select_id);
    let (event, flavor) = RECORDED_INDICES.get(& key).
        expect(&format!("Unable to fetch key: {:?}", key));

    trace!("Event fetched: {:?}", event);
    (event, *flavor)
}
