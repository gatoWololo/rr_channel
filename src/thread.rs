use std::cell::RefCell;
use log::{trace, info, error};
use std::thread;
use std::thread::JoinHandle;
pub use std::thread::{current, yield_now, sleep, panicking, park, park_timeout};

pub fn get_det_id() -> DetThreadId {
    DET_ID.with(|di| di.borrow().clone())
}

pub fn get_select_id() -> u32 {
    SELECT_ID.with(|id| *id.borrow())
}

/// TODO I want to express that this is mutable somehow.
pub fn inc_select_id() {
    SELECT_ID.with(|id| {
        *id.borrow_mut() += 1;
    });
}

thread_local! {
    /// TODO this id is probably redundant.
    static SELECT_ID: RefCell<u32> = RefCell::new(0);

    static DET_ID_SPAWNER: RefCell<DetIdSpawner> = RefCell::new(DetIdSpawner::new());
    /// Unique threadID assigned at thread spawn to to each thread.
    static DET_ID: RefCell<DetThreadId> = RefCell::new(DetThreadId::new());
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
    trace!("spawn: Assigned determinsitic id {:?} for new thread.", new_id);

    thread::spawn(|| {
        // Initialize TLS for this thread.
        DET_ID.with(|id| {
            *id.borrow_mut() = new_id;
        });
        DET_ID_SPAWNER.with(|spawner| {
            *spawner.borrow_mut() = new_spawner;
        });

        // SELECT_ID is fine starting at 0.
        f()
    })
}

use serde::{Serialize, Deserialize};

pub struct DetIdSpawner {
    pub child_index: u32,
    pub thread_id: DetThreadId,
}

impl DetIdSpawner {
    pub fn new() -> DetIdSpawner {
        DetIdSpawner { child_index: 0, thread_id: DetThreadId::new()  }
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

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct DetThreadId {
    thread_id: Vec<u32>,
}

impl DetThreadId {
    pub fn new() -> DetThreadId {
        DetThreadId { thread_id: vec![] }
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
                // println!("({}): {:?}", i, get_det_id());

                let mut v2 = vec![];
                for j in 0..4 {
                    let h2 = spawn(move ||{
                        let a = [i, j];
                        assert_eq!(DetThreadId::from(&a[..]), get_det_id());
                        // println!("({}, {}): Det Thread Id: {:?}", i, j, get_det_id());
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
        trace!("Builder: Assigned determinsitic id {:?} for new thread.", new_id);

        self.builder.spawn(|| {
            // Initialize TLS for this thread.
            DET_ID.with(|id| {
                *id.borrow_mut() = new_id;
            });

            DET_ID_SPAWNER.with(|spawner| {
                *spawner.borrow_mut() = new_spawner;
            });

            // SELECT_ID is fine starting at 0.
            f()
        })
    }
}
