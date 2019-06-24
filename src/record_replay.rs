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
use crossbeam_channel::RecvError;
use crate::det_id::get_det_id;
use crate::det_id::get_select_id;
use crate::det_id::inc_select_id;
use serde::{Serialize, Deserialize};

// TODO use environment variables to generalize this.
const LOG_FILE_NAME: &str = "/home/gatowololo/det_file.txt";

/// Unique Identifier for entries in our log. Useful for easy serialize/deserialize
/// into our log.

/// We determinize not only select!() but also MPSC channels. On select!() we need the
/// index of the channel to read from. On direct .recv() from MPSC channels we just
/// read directly.
#[derive(Serialize, Deserialize, Debug)]
pub enum ReceiveType {
    Select(u32),
    DirectChannelRecv,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LogEntry {
    pub current_thread: DetThreadId,
    /// For multiple producer channels we need to diffentiate who the sender was.
    /// As the index only tells us the correct receiver end, but not who the sender was.
    pub sender_thread: Option<DetThreadId>,
    /// Unique per-thread identifier given to every select dynamic instance.
    /// (current_thread, select_id) form a unique key per entry in our map and log.
    pub select_id: u32,
    /// Index of selected channel for select!()
    pub index: ReceiveType,
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
    /// Maps (our_thread: DetThreadId, SELECT_ID: u32) ->
    ///      (index: u32, sender_thread: DetThreadId)
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (ReceiveType, Option<DetThreadId>)> = {
        trace!("Initializing RECORDED_INDICES lazy static.");
        use std::io::BufReader;

        let mut recorded_indices = HashMap::new();
        let log = File::open(LOG_FILE_NAME).
            expect(& format!("Unable to open {} for replay.", LOG_FILE_NAME));
        let log = BufReader::new(log);

        for line in log.lines() {
            let line = line.expect("Unable to read recorded log file");
            let entry: LogEntry = serde_json::from_str(&line).
                expect("Malformed log entry.");
            recorded_indices.insert((entry.current_thread, entry.select_id),
                                    (entry.index, entry.sender_thread));
        }

        trace!("{:?}", recorded_indices);
        recorded_indices
    };
}

/// Unwrap rr_channels return value from calling .recv() and log results. Should only
/// be called in Record Mode.
pub fn get_message_and_log<T>(index: ReceiveType,
                              received: Result<(Option<DetThreadId>, T), RecvError>)
                              -> Result<T, RecvError> {
    use std::io::Write;

    let (sender_thread, msg) : (Option<DetThreadId>, Result<T, RecvError>) =
        match received {
            // Err(e) on the RHS is not the same type as Err(e) LHS.
            Err(e) => (None, Err(e)),

            Ok((sender_thread, msg)) => {
                if sender_thread.is_none() {
                    panic!("sender_thread expected on Record mode.");
                }
                (sender_thread, Ok(msg))
            }
        };

    // Write our (DET_ID, SELECT_ID) -> index to our log file
    let current_thread = get_det_id();
    let select_id = get_select_id();
    let entry: LogEntry = LogEntry { current_thread, sender_thread,
                                     select_id , index };
    let serialized = serde_json::to_string(&entry).unwrap();

    WRITE_LOG_FILE.
        lock().
        unwrap().
        write_fmt(format_args!("{}\n", serialized)).
        expect("Unable to write to log file.");

    debug!("Logged entry: {:?}", entry);
    inc_select_id();
    msg
}

pub fn get_log_entry<'a>(our_thread: DetThreadId, select_id: u32)
                     -> (&'a ReceiveType, &'a Option<DetThreadId>) {
    trace!("Replaying for our_thread: {:?}, sender_thread {:?}",
           our_thread, select_id);
    let (index, sender_id) = RECORDED_INDICES.get(& (our_thread, select_id)).
        expect("Unable to fetch key.");

    trace!("Index fetched: {:?}", index);
    trace!("Sender Id fetched: {:?}", sender_id);

    (index, sender_id)
}
