use log::Level::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::thread::JoinHandle;
pub use std::thread::{current, panicking, park, park_timeout, sleep, yield_now};
use std::fmt::Debug;
use serde::{Deserialize, Serialize};

use crate::{error, recordlog, desync};

pub fn get_det_id() -> DetThreadId {
    DET_ID.with(|di| di.borrow().clone())
}

/// Like `get_det_id` but treats missing DetThreadId as a desynchonization error.
/// NO LONGER NEEDED.
// pub fn get_det_id_desync() -> DetThreadId {
//     match get_det_id() {
//         Some(v) => Ok(v),
//         None => Err(error::DesyncError::UnitializedDetThreadId),
//     }
// }

pub fn set_det_id(new_id: DetThreadId) {
    DET_ID.with(|id| {
        *id.borrow_mut() = new_id;
    });
}

pub fn get_temp_det_id() -> DetThreadId {
    TEMP_DET_ID.with(|di| di.borrow().clone().expect("No ID set!"))
}

pub fn set_temp_det_id(new_id: DetThreadId) {
    TEMP_DET_ID.with(|id| {
        *id.borrow_mut() = Some(new_id);
    });
}

pub fn get_event_id() -> recordlog::EventId {
    EVENT_ID.with(|id| *id.borrow())
}

/// TODO I want to express that this is mutable somehow.
pub fn inc_event_id() {
    EVENT_ID.with(|id| {
        *id.borrow_mut() += 1;
        crate::log_rr!(Debug, "Incremented TLS event_id: {:?}", *id.borrow());
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
    /// Unique threadID assigned at thread spawn to each thread.
    pub static DET_ID: RefCell<DetThreadId> = RefCell::new(DetThreadId::new());
    /// Tells us whether this thread has been initialized yet. Init happens either through use of
    /// our det thread spawning wrappers or through explicit call to `init_tivo_thread_root`. This
    /// allows us to tell if a thread was spawned outside of our API. When this is false, the first
    /// call to DET_ID will error. This works because DET_ID is initialized lazily.
    static THREAD_INITIALIZED: RefCell<bool> = RefCell::new(false);

    static CHANNEL_ID: AtomicU32 = AtomicU32::new(0);
    /// Hack to know when we're in the router. Forwading the DetThreadId to to the callback
    /// by temporarily setting DET_ID to a different value.
    static FORWADING_ID: RefCell<bool> = RefCell::new(false);
    /// DetId set when forwarding id. Should only be accessed if in_forwading() returns
    /// true. Which is set by start_forwarding_id() and ends by stop_forwading_id().
    pub static TEMP_DET_ID: RefCell<Option<DetThreadId>> = RefCell::new(None);
}

pub fn start_forwading_id(forwarding_id: DetThreadId) {
    FORWADING_ID.with(|fi| {
        *fi.borrow_mut() = true;
    });
    set_temp_det_id(forwarding_id);
}

pub fn in_forwarding() -> bool {
    FORWADING_ID.with(|fi| *fi.borrow())
}

pub fn stop_forwarding_id(original_id: DetThreadId) {
    FORWADING_ID.with(|fi| {
        *fi.borrow_mut() = false;
    });
    set_temp_det_id(original_id);
}

/// Wrapper around thread::spawn. We will need this later to assign a deterministic thread id, and
/// allow each thread to have access to its ID and a local dynamic select counter through TLS.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
    where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Send + 'static,
{
    let new_id = DET_ID_SPAWNER.with(|spawner| spawner.borrow_mut().new_child_det_id());
    crate::log_rr!(
        Info,
        "std::thread::spawn Assigned determinsitic id {:?} for new thread.",
        new_id
    );

    thread::spawn(|| {
        THREAD_INITIALIZED.with(|ti| {
            ti.replace(true);
        });

        // Initialize TLS for this thread.
        DET_ID.with(|id| {
            *id.borrow_mut() = new_id;
        });
        DET_ID_SPAWNER.with(|spawner| {
            // Initalizes DET_ID_SPAWNER based on the value just set for DET_ID.
        });

        // EVENT_ID will be initalized to zero on first usage.
        f()
    })
}

pub struct DetIdSpawner {
    pub child_index: u32,
    pub thread_id: DetThreadId,
}

impl DetIdSpawner {
    pub fn starting() -> DetIdSpawner {
        DetIdSpawner {
            child_index: 0,
            thread_id: DET_ID.with(|id| {
                // This happens when the current thread was initialized without using our
                // deterministic thread spawning API.
                let e = "This thread did not have its DetThreadId initalized";
                id.borrow().clone()
            }),
        }
    }

    pub fn new_child_det_id(&mut self) -> DetThreadId {
        let mut new_thread_id = self.thread_id.clone();
        new_thread_id.extend_path(self.child_index);
        self.child_index += 1;
        new_thread_id
    }
}

/// Every thread is assigned a deterministic thread id (DTI) if the thread is spawned via our API.
/// This value should be deterministic even across executions of the program. Assuming the same
/// number of threads are spawned every execution. We don't really check this assumption, but notice
/// that if this ever doesn't hold true the program will quickly diverge during replay. Thus it
/// is sorta self checking? Then again it would be nice to test for this and fail quickly with a
/// good error. TODO we probably want to check channel creation (maybe even destruction) events.
#[derive(Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct DetThreadId {
    thread_id: [u32; DetThreadId::MAX_SIZE],
    size: usize,
}

impl DetThreadId {
    const MAX_SIZE: usize = 10;

    /// TODO DOCUMENT. Also should this be pub? That looks wrong...
    pub fn new() -> DetThreadId {
        THREAD_INITIALIZED.with(|ti| {
            if !*ti.borrow() {
                panic!("thread not initialized");
            }
        });
        DetThreadId {
            thread_id: [0; DetThreadId::MAX_SIZE],
            size: 0,
        }
    }

    fn extend_path(&mut self, node: u32) {
        if self.size < DetThreadId::MAX_SIZE {
            self.thread_id[self.size] = node;
            self.size += 1;
        }
        else {
            panic!("Cannot extend path. Thread tree too deep.");
        }
    }

    #[allow(dead_code)] // Used for testing.
    fn as_slice(&self) -> &[u32] {
        &self.thread_id[0..self.size]
    }
}

impl Debug for DetThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        // Only print up to channel size.
        write!(f, "ThreadId{:?}", &self.thread_id[0..self.size])
    }
}

/// Wrapper around std::thread::Builder API but with deterministic ID assignment.
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
        crate::log_rr!(
            Info,
            "std::thread::Builder::spawn: Assigned deterministic id {:?} for new thread.",
            new_id
        );

        self.builder.spawn(|| {
            THREAD_INITIALIZED.with(|ti| {
                ti.replace(true);
            });
            // Set DET_ID to correct id! This is now safe as we have set THREAD_INITIALIZED To true.
            DET_ID.with(|id| {
                *id.borrow_mut() = new_id;
            });
            // Inits!
            DET_ID_SPAWNER;

            // EVENT_ID will be initalized to zero on first usage.
            f()
        })
    }
}

/// Because of the router, we want to "forward" the original sender's DetThreadId
/// sometimes. We should always use this function when sending our DetThreadId via a
/// sender channel as it handles the router and non-router cases.
pub fn get_forwarding_id() -> DetThreadId {
    if in_forwarding() {
        get_temp_det_id()
    } else {
        get_det_id()
    }
}

pub fn init_tivo_thread_root() {
    THREAD_INITIALIZED.with(|ti| {
        ti.replace(true);
    });
    // Init!
    DET_ID;
}
#[cfg(test)]
mod tests {
    use std::time::Duration;
    use rand::{Rng, thread_rng};
    use crate::detthread::init_tivo_thread_root;

    #[test]
    /// TODO Add random delays to increase amount of nondeterminism.
    /// Create a thread tree and ensure the threads are given IDs according to their index.
    fn deterministic_ids() {
        init_tivo_thread_root();
        use super::{get_det_id, spawn};
        use std::thread::JoinHandle;

        let mut v1: Vec<JoinHandle<_>> = vec![];

        for i in 0..4 {
            let h1 = spawn(move || {
                // Wait a random amount of time, so that threads spawned after us have a chance
                // to spawn their children first.
                let n = thread_rng().gen_range(1, 15);
                std::thread::sleep(Duration::from_millis(n));

                let a = [i];
                assert_eq!(&a[..], get_det_id().as_slice());

                let mut v2 = vec![];
                for j in 0..4 {
                    let h2 = spawn(move || {
                        let a = [i, j];
                        assert_eq!(&a[..], get_det_id().as_slice());
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
