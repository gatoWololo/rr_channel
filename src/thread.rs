use log::{error, info, trace};
use std::cell::RefCell;
use std::thread;
use std::thread::JoinHandle;
pub use std::thread::{current, panicking, park, park_timeout, sleep, yield_now};
// use backtrace::Backtrace;
use crate::log_trace;
use std::sync::atomic::{AtomicU32, Ordering};
use crate::record_replay::EventId;

pub fn get_det_id() -> Option<DetThreadId> {
    DET_ID.with(|di| {
        di.borrow().clone()
    })
}

pub fn set_det_id(new_id: Option<DetThreadId>) {
    DET_ID.with(|id| {
        *id.borrow_mut() = new_id;
    });
}

pub fn get_temp_det_id() -> Option<DetThreadId> {
    TEMP_DET_ID.with(|di| {
        di.borrow().clone()
    })
}

pub fn set_temp_det_id(new_id: Option<DetThreadId>) {
    TEMP_DET_ID.with(|id| {
        *id.borrow_mut() = new_id;
    });
}


pub fn get_event_id() -> EventId {
    EVENT_ID.with(|id| *id.borrow())
}

/// TODO I want to express that this is mutable somehow.
pub fn inc_event_id() {
    EVENT_ID.with(|id| {
        *id.borrow_mut() += 1;
        log_trace(&format!("Incremented TLS event_id: {:?}", *id.borrow()));
    });
}

pub fn get_and_inc_channel_id() -> u32 {
    CHANNEL_ID.with(|ci| ci.fetch_add(1, Ordering::SeqCst))
}

thread_local! {
    /// Unique ID to keep track of events. Not strictly necessary but extremely
    /// helpful for debugging and sanity.
    static EVENT_ID: RefCell<u32> = RefCell::new(0);

    static DET_ID_SPAWNER: RefCell<DetIdSpawner> = RefCell::new(DetIdSpawner::starting());
    /// Unique threadID assigned at thread spawn to to each thread.
    pub static DET_ID: RefCell<Option<DetThreadId>> = RefCell::new(DetThreadId::new());
    static CHANNEL_ID: AtomicU32 = AtomicU32::new(0);
    /// Hack to know when we're in the router. Forwading the DetThreadId to to the callback
    /// by temporarily setting DET_ID to a different value.
    static FORWADING_ID: RefCell<bool> = RefCell::new(false);
    /// DetId set when forwading id. Should only be accessed if in_forwading() returns
    /// true. Which is set by start_forwading_id() and ends by stop_forwading_id()
    pub static TEMP_DET_ID: RefCell<Option<DetThreadId>> = RefCell::new(DetThreadId::new());
}

pub fn start_forwading_id(forwarding_id: Option<DetThreadId>) {
    FORWADING_ID.with(|fi| {
        *fi.borrow_mut() = true;
    });
    set_temp_det_id(forwarding_id);
}

pub fn in_forwarding() -> bool {
    FORWADING_ID.with(|fi| *fi.borrow())
}

pub fn stop_forwarding_id(original_id: Option<DetThreadId>) {
    FORWADING_ID.with(|fi| {
        *fi.borrow_mut() = false;
    });
    set_temp_det_id(original_id);
}

/// Wrapper around thread::spawn. We will need this later to assign a
/// deterministic thread id, and allow each thread to have access to its ID and
/// a local dynamic select counter through TLS.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    let new_id = DET_ID_SPAWNER.with(|spawner| spawner.borrow_mut().new_child_det_id());
    let new_spawner = DetIdSpawner::from(new_id.clone());
    log_trace(&format!(
        "thread::spawn() Assigned determinsitic id {:?} for new thread.",
        new_id
    ));

    thread::spawn(|| {
        // Initialize TLS for this thread.
        DET_ID.with(|id| {
            *id.borrow_mut() = Some(new_id);
        });
        DET_ID_SPAWNER.with(|spawner| {
            *spawner.borrow_mut() = new_spawner;
        });

        // EVENT_ID is fine starting at 0.
        f()
    })
}

use serde::{Deserialize, Serialize};

pub struct DetIdSpawner {
    pub child_index: u32,
    pub thread_id: DetThreadId,
}

impl DetIdSpawner {
    pub fn starting() -> DetIdSpawner {
        DetIdSpawner {
            child_index: 0,
            thread_id: DetThreadId { thread_id: vec![] },
        }
    }

    pub fn new_child_det_id(&mut self) -> DetThreadId {
        let mut new_thread_id = self.thread_id.clone();
        new_thread_id.extend_path(self.child_index);
        self.child_index += 1;
        new_thread_id
    }
}

impl From<DetThreadId> for DetIdSpawner {
    fn from(thread_id: DetThreadId) -> DetIdSpawner {
        DetIdSpawner {
            child_index: 0,
            thread_id,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct DetThreadId {
    thread_id: Vec<u32>,
}

use std::fmt::Debug;
impl Debug for DetThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "ThreadId{:?}", self.thread_id)
    }
}

impl DetThreadId {
    pub fn new() -> Option<DetThreadId> {
        // The main thread get initialized here. Every other thread should be
        // assigned a DetThreadId through the thread/Builder spawn wrappers.
        // This allows to to tell if a thread was spawned through other means
        // (not our API wrapper).
        if Some("main") == thread::current().name() {
            Some(DetThreadId { thread_id: vec![] })
        } else {
            None
        }
    }

    fn extend_path(&mut self, node: u32) {
        self.thread_id.push(node);
    }
}

impl From<&[u32]> for DetThreadId {
    fn from(thread_id: &[u32]) -> DetThreadId {
        DetThreadId {
            thread_id: Vec::from(thread_id),
        }
    }
}

pub struct Builder {
    builder: std::thread::Builder,
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            builder: std::thread::Builder::new(),
        }
    }

    pub fn name(self, name: String) -> Builder {
        Builder {
            builder: self.builder.name(name),
        }
    }

    pub fn stack_size(self, size: usize) -> Builder {
        Builder {
            builder: self.builder.stack_size(size),
        }
    }

    pub fn spawn<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Send + 'static,
    {
        let new_id = DET_ID_SPAWNER.with(|spawner| spawner.borrow_mut().new_child_det_id());

        let new_spawner = DetIdSpawner::from(new_id.clone());
        log_trace(&format!(
            "Builder: Assigned determinsitic id {:?} for new thread.",
            new_id
        ));

        self.builder.spawn(|| {
            // Initialize TLS for this thread.
            DET_ID.with(|id| {
                *id.borrow_mut() = Some(new_id);
            });

            DET_ID_SPAWNER.with(|spawner| {
                *spawner.borrow_mut() = new_spawner;
            });

            // EVENT_ID is fine starting at 0.
            f()
        })
    }
}

mod tests {
    #[test]
    /// TODO Add random delays to increase amount of nondeterminism.
    fn determinitic_ids() {
        use super::{get_det_id, spawn, DetThreadId};
        use std::thread::JoinHandle;

        let mut v1: Vec<JoinHandle<_>> = vec![];

        for i in 0..4 {
            let h1 = spawn(move || {
                let a = [i];
                assert_eq!(DetThreadId::from(&a[..]), get_det_id());

                let mut v2 = vec![];
                for j in 0..4 {
                    let h2 = spawn(move || {
                        let a = [i, j];
                        assert_eq!(DetThreadId::from(&a[..]), get_det_id());
                    });
                    v2.push(h2);
                }
                for handle in v2 {
                    handle.join().unwrap();
                }
            });
            v1.push(h1);
        }
        for handle in v1 {
            handle.join().unwrap();
        }

        assert!(true);
    }
}
