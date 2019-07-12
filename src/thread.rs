use std::cell::RefCell;
use log::{trace, info, error};
use std::thread;
use std::thread::JoinHandle;
pub use std::thread::{current, yield_now, sleep, panicking, park, park_timeout};
// use backtrace::Backtrace;
use std::sync::atomic::{AtomicU32, Ordering};
use crate::log_trace;

pub fn get_det_id() -> DetThreadId {
    DET_ID.with(|di| di.borrow().as_ref().
                expect("thread_id not initialized").clone())
}

// pub fn get_det_id() -> DetThreadId {
//     DET_ID.with(|di| {
//         match di.borrow().as_ref() {
//             None => {
//                 log_trace("OMAR HERE");
//                 panic!(format!("{:?}", Backtrace::new()));
//             }
//             Some(di) => di.clone(),
//         }
//     })
// }

pub fn get_det_id_clone() -> Option<DetThreadId> {
    DET_ID.with(|di| {
        di.borrow().clone()
    })
}

pub fn get_select_id() -> u32 {
    EVENT_ID.with(|id| *id.borrow())
}

/// TODO I want to express that this is mutable somehow.
pub fn inc_select_id() {
    EVENT_ID.with(|id| {
        *id.borrow_mut() += 1;
    });
}

pub fn get_and_inc_channel_id() -> u32 {
    CHANNEL_ID.with(|ci|{
        ci.fetch_add(1, Ordering::SeqCst)
    })

}

thread_local! {
    /// TODO this id is probably redundant.
    static EVENT_ID: RefCell<u32> = RefCell::new(0);

    static DET_ID_SPAWNER: RefCell<DetIdSpawner> = RefCell::new(DetIdSpawner::starting());
    /// Unique threadID assigned at thread spawn to to each thread.
    static DET_ID: RefCell<Option<DetThreadId>> = RefCell::new(DetThreadId::new());
    ///
    static CHANNEL_ID: AtomicU32 = AtomicU32::new(0);
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
    let new_id = DET_ID_SPAWNER.with(|spawner| {
        spawner.borrow_mut().new_child_det_id()
    });
    let new_spawner = DetIdSpawner::from(new_id.clone());
    log_trace(&format!("thread::spawn() Assigned determinsitic id {:?} for new thread.", new_id));

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

use serde::{Serialize, Deserialize};

pub struct DetIdSpawner {
    pub child_index: u32,
    pub thread_id: DetThreadId,
}

impl DetIdSpawner {
    pub fn starting() -> DetIdSpawner {
        DetIdSpawner { child_index: 0, thread_id: DetThreadId{ thread_id: vec![] } }
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
        DetIdSpawner { child_index: 0, thread_id }
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
        if(Some("main") == thread::current().name()) {
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
    fn from(thread_id :&[u32]) -> DetThreadId {
        DetThreadId { thread_id: Vec::from(thread_id) }
    }
}

pub struct Builder {
    builder: std::thread::Builder
}

impl Builder {
    pub fn new() -> Builder {
        Builder { builder: std::thread::Builder::new() }
    }


    pub fn name(self, name: String) -> Builder {
        Builder { builder: self.builder.name(name) }
    }


    pub fn stack_size(self, size: usize) -> Builder {
        Builder { builder: self.builder.stack_size(size) }
    }


    pub fn spawn<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>> where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Send + 'static, {
        let new_id = DET_ID_SPAWNER.with(|spawner| {
            spawner.borrow_mut().new_child_det_id()
        });

        let new_spawner = DetIdSpawner::from(new_id.clone());
        log_trace(&format!("Builder: Assigned determinsitic id {:?} for new thread.", new_id));

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
        use super::{spawn, get_det_id, DetThreadId};
        use std::thread::JoinHandle;

        let mut v1: Vec<JoinHandle<_>> = vec![];

        for i in 0..4 {
            let h1 = spawn(move || {
                let a = [i];
                assert_eq!(DetThreadId::from(&a[..]), get_det_id());

                let mut v2 = vec![];
                for j in 0..4 {
                    let h2 = spawn(move ||{
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