use log::Level::{Debug, Warn};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DesyncMode {
    Panic,
    KeepGoing,
}

pub(crate) fn program_desyned() -> bool {
    DESYNC.load(Ordering::SeqCst)
}

pub(crate) fn mark_program_as_desynced() {
    if !DESYNC.load(Ordering::SeqCst) {
        crate::log_rr!(Warn, "Program marked as desynced!");
        DESYNC.store(true, Ordering::SeqCst);
        wake_up_threads();
    }
}

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

#[test]
fn condvar_test() {
    let mut handles = vec![];
    for i in 1..10 {
        let h = thread::spawn(move || {
            println!("Putting thread {} to sleep.", i);
            sleep_until_desync();
            println!("Thread {} woke up!", i);
        });
        handles.push(h);
    }

    thread::sleep_ms(1000);
    println!("Main thread waking everyone up...");
    wake_up_threads();
    for h in handles {
        h.join().unwrap();
    }
    assert!(true);
}
