//! Desynchronizations are common with RR channels (as of now).
//! This module handles all desyncing behavior including sleeping threads,
//! waking up threads.
//! We detect desyncs by tagging the recordlog event with metadata about the
//! type of message we expect from the type of channel we expect it. If there
//! is ever a mismatch between the type of message we expect, and the real message,
//! we have desynchronized.
use log::Level::{Debug, Warn};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex};
use crate::error::DesyncError;
use crate::{detthread, DetMessage};
use std::cell::RefMut;
use crate::BufferedValues;
use crate::DESYNC_MODE;

/// Wrapper around DesyncError.
pub(crate) type Result<T> = std::result::Result<T, DesyncError>;

/// Through the envvar RR_DESYNC_MODE users can select what action should
/// happen when we encounter a desynchronization.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DesyncMode {
    Panic,
    KeepGoing,
}

/// Desync can happen from any thread at any time. This global DESYNC knows
/// if anyone has desynced. So we can dynamically make choices about what to
/// do if we're desyced.
pub(crate) fn program_desyned() -> bool {
    DESYNC.load(Ordering::SeqCst)
}

/// Inform system that we have detected a desynchronization. This wakes up
/// any currently sleeping threads. See `sleep_until_desync` for more information.
pub(crate) fn mark_program_as_desynced() {
    if !DESYNC.load(Ordering::SeqCst) {
        crate::log_rr!(Warn, "Program marked as desynced!");
        DESYNC.store(true, Ordering::SeqCst);
        wake_up_threads();
    }
}

/// This thread has fallen off the end of the log. We assume this thread "didn't make it
/// this far" on the record execution. So we put it to sleep. When we desynchronize this thread
/// will be waken up. This is dones via conditional variables.
pub(crate) fn sleep_until_desync() {
    crate::log_rr!(Debug, "Thread being put to sleep.");
    let (lock, condvar) = &*PARK_THREADS;
    let mut started = lock.lock().expect("Unable to lock mutex.");
    while !*started {
        started = condvar.wait(started).expect("Unable to wait on condvar.");
    }
    crate::log_rr!(Debug, "Thread woke up from eternal slumber!");
}

pub(crate) fn wake_up_threads() {
    crate::log_rr!(Debug, "Waking up threads!");
    let (lock, condvar) = &*PARK_THREADS;
    let mut started = lock.lock().expect("Unable to lock mutex.");
    *started = true;
    condvar.notify_all();
}

lazy_static::lazy_static! {
    /// When we run off the end of the log,
    /// if we're not DESYNC, we assume that thread blocked forever during record
    /// if any other thread ever DESYNCs we let the thread continue running
    /// to avoid deadlocks. This condvar implements that logic.
    pub static ref PARK_THREADS: (Mutex<bool>, Condvar) = {
        (Mutex::new(false), Condvar::new())
    };

    /// Global bool keeping track if any thread has desynced.
    /// TODO: This may be too heavy handed. If we remove this bool, we get
    /// per event desync checks, which may be better than a global concept?
    pub static ref DESYNC: AtomicBool = {
        AtomicBool::new(false)
    };
}

/// Handles desynchronization events based on global DESYNC_MODE.
pub(crate) fn handle_desync<T, E>(
    desync: DesyncError,
    recv_msg: impl Fn() -> ::std::result::Result<DetMessage<T>, E>,
    mut buffer: RefMut<BufferedValues<T>>,
) ->  ::std::result::Result<T, E> {
    crate::log_rr!(Warn, "Desynchonization found: {:?}", desync);

    match *DESYNC_MODE {
        DesyncMode::KeepGoing => {
            mark_program_as_desynced();
            detthread::inc_event_id();

            // Try using entry from buffer before recv()-ing directly from receiver. We don't
            // care who the expected sender was. Any value will do.
            for queue in (*buffer).values_mut() {
                if let Some(val) = queue.pop_front() {
                    return Ok(val);
                }
            }
            // No entries in buffer. Read from the wire.
            recv_msg().map(|t| t.1)
        }
        DesyncMode::Panic => {
            panic!("Desynchonization found: {:?}", desync);
        }
    }
}

#[cfg(test)]
mod test {
    use crate::desync::{sleep_until_desync, wake_up_threads};
    use std::time::Duration;
    use crate::detthread::init_tivo_thread_root;

    #[test]
    fn condvar_test() {
        init_tivo_thread_root();
        let mut handles = vec![];
        for i in 1..10 {
            let h = crate::detthread::spawn(move || {
                // println!("Putting thread {} to sleep.", i);
                sleep_until_desync();
                // println!("Thread {} woke up!", i);
            });
            handles.push(h);
        }

        //TODO Is this enough time?
        std::thread::sleep(Duration::from_millis(1));
        // println!("Main thread waking everyone up...");
        wake_up_threads();
        for h in handles {
            h.join().unwrap();
        }
        assert!(true);
    }
}
