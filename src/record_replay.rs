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

// TODO use environment variables to generalize this.
const LOG_FILE_NAME: &str = "/home/gatowololo/det_file.txt";

use serde::{Serialize, Deserialize};
/// Unique Identifier for entries in our log. Useful for easy serialize/deserialize
/// into our log.
#[derive(Serialize, Deserialize, Debug)]
pub struct LogEntry {
    pub current_thread: DetThreadId,
    /// For multiple producer channels we need to diffentiate who the sender was.
    /// As the index only tells us the correct receiver end, but not who the sender was.
    pub sender_thread: Option<DetThreadId>,
    pub select_id: u32,
    pub index: u32
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
    /// TODO: Use type aliases to show this.
    pub static ref RECORDED_INDICES: HashMap<(DetThreadId, u32), (u32, Option<DetThreadId>)> = {
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
