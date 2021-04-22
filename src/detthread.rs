use std::cell::RefCell;
use std::fmt::Debug;
use std::sync::atomic::AtomicU32;
use std::thread::JoinHandle;
pub use std::thread::{current, panicking, park, park_timeout, sleep, yield_now};

use crate::recordlog::{ChannelVariant, TivoEvent};
use crate::rr::{DetChannelId, RecordEventChecker};
use crate::{get_rr_mode, recordlog, EventRecorder, RRMode};
use serde::{Deserialize, Serialize};
use tracing::{error, event, info, span, Level};

thread_local! {
    /// DET_ID must be set before accessing the DET_ID_SPAWNER as it relies on DET_ID for correct
    /// behavior.
    pub(crate) static DET_ID_SPAWNER: RefCell<DetIdSpawner> = RefCell::new(DetIdSpawner::starting());
    /// Unique threadID assigned at thread spawn to each thread.
    pub static DET_ID: RefCell<DetThreadId> = RefCell::new(DetThreadId::new());
    /// Tells us whether this thread has been initialized yet. Init happens either through use of
    /// our det thread spawning wrappers or through explicit call to `init_tivo_thread_root_test`. This
    /// allows us to tell if a thread was spawned outside of our API. When this is false, the first
    /// call to DET_ID will error. This works because DET_ID is initialized lazily.
    pub(crate) static THREAD_INITIALIZED: RefCell<bool> = RefCell::new(false);

    pub(crate) static CHANNEL_ID: AtomicU32 = AtomicU32::new(1);
}

pub(crate) fn get_det_id() -> DetThreadId {
    DET_ID.with(|di| di.borrow().clone())
}

/// Ask parent thread to generate an ID for new child thread. Should be called by parent before
/// the thread is spawned. This ID should be passed to the child via the closure parameter for
/// spawn. See Builder::spawn above for an example.
pub fn generate_new_child_id() -> DetThreadId {
    DET_ID_SPAWNER.with(|spawner| spawner.borrow_mut().new_child_det_id())
}

/// Sometimes it can't be helped to spawn threads outside of our API. This method may be called
/// as the first thing from the closure that will be passed to a library or function that will
/// spawn a foreign thread, a thread spawned outside of our Tivo API.
pub fn init_foreign_thread(child_id: DetThreadId) {
    THREAD_INITIALIZED.with(|ti| {
        if *ti.borrow() {
            error!("Thread already initialized with {:?}", get_det_id());
            panic!("Thread already initialized with {:?}", get_det_id())
        }
    });
    info!("Foreign Thread assigned DTI: {:?}", child_id);
    initialize_new_thread(child_id);
}

/// Wrapper around thread::spawn. We will need this later to assign a deterministic thread id, and
/// allow each thread to have access to its ID and a local dynamic select counter through TLS.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    Builder::new().spawn(f).unwrap()
}

/// Every thread has a DET_ID which holds its current det id. We also need a per-thread det id
/// spawner which keeps track of what ID to assign to this threads next child thread.
pub struct DetIdSpawner {
    /// One-based indexing. We reserve zero as an "uninitialized state"
    pub child_index: u32,
    pub thread_id: DetThreadId,
}

impl DetIdSpawner {
    pub fn starting() -> DetIdSpawner {
        DetIdSpawner {
            child_index: 1,
            thread_id: DET_ID.with(|id| id.borrow().clone()),
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
    pub(crate) fn new() -> DetThreadId {
        THREAD_INITIALIZED.with(|ti| {
            if !*ti.borrow() {
                error!("thread not initialized");
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
        } else {
            panic!("Cannot extend path. Thread tree too deep.");
        }
    }

    #[cfg(test)]
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

impl From<&[u32]> for DetThreadId {
    fn from(slice: &[u32]) -> Self {
        let mut dti = DetThreadId {
            thread_id: [0; DetThreadId::MAX_SIZE],
            size: 0,
        };

        for e in slice {
            dti.extend_path(*e);
        }
        dti
    }
}

impl Default for DetThreadId {
    fn default() -> Self {
        DetThreadId::new()
    }
}
/// Wrapper around std::thread::Builder API but with deterministic ID assignment.
pub struct Builder {
    builder: std::thread::Builder,
}

struct ThreadSpawnChecker {
    thread_id: DetThreadId,
}

// We don't care about the () here.
impl RecordEventChecker<()> for ThreadSpawnChecker {
    fn check_recorded_event(&self, re: &TivoEvent) -> Result<(), TivoEvent> {
        let err_case = Err(TivoEvent::ThreadInitialized(self.thread_id.clone()));
        match re {
            TivoEvent::ThreadInitialized(expected_dti) => {
                if *expected_dti != self.thread_id {
                    return err_case;
                }
                return Ok(());
            }
            _ => return err_case,
        }
    }
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
        // Spans are thread-local, to it just works(TM) that this span is technically covering the
        // entire execution of the child thread
        let _e = span!(Level::INFO, "detthread::spawn()").entered();
        let new_id = generate_new_child_id();
        let recorder = EventRecorder::get_global_recorder();

        match get_rr_mode() {
            RRMode::Record => {
                let event = TivoEvent::ThreadInitialized(new_id.clone());

                // TODO: We shouldn't have metadata for thread events this requires a refactoring
                // of the recordlog entries.
                let metadata = recordlog::RecordMetadata::new(
                    "Thread".to_string(),
                    ChannelVariant::None,
                    DetChannelId::fake(),
                );
                recorder.write_event_to_record(event, &metadata).unwrap();
            }
            RRMode::Replay => {
                match recorder.get_log_entry() {
                    Ok(event) => {
                        let tsc = ThreadSpawnChecker {
                            thread_id: new_id.clone(),
                        };
                        let metadata = recordlog::RecordMetadata::new(
                            "Thread".to_string(),
                            ChannelVariant::None,
                            DetChannelId::fake(),
                        );

                        // Can't propagate error up, panic.
                        if let Err(e) = tsc.check_event_mismatch(&event, &metadata) {
                            panic!("{}", e);
                        }
                    }
                    Err(e) => {
                        error!(%e);
                        panic!("{}", e);
                    }
                }
            }
            RRMode::NoRR => {}
        }

        self.builder.spawn(move || {
            initialize_new_thread(new_id.clone());
            let t = std::thread::current();

            let _e = span!(Level::INFO,
              "Thread",
              dti= ? get_det_id(),
              name=t.name().unwrap_or("None"))
            .entered();
            event!(Level::INFO, "New Thread Spawned!");
            let h = f();
            event!(Level::INFO, "Thread Finished.");
            h
        })
    }
}

impl Default for Builder {
    fn default() -> Self {
        Builder::new()
    }
}

/// Set all necessary state for newly spawned thread. This function should only be called from
/// inside the new child thread.
fn initialize_new_thread(new_id: DetThreadId) {
    THREAD_INITIALIZED.with(|ti| {
        ti.replace(true);
    });
    // Set DET_ID to correct id!
    // Accessing DET_ID won't panic anymore as we have set THREAD_INITIALIZED to true.
    // DET_ID must be set before accessing the DET_ID_SPAWNER as it relies on DET_ID for correct
    // behavior.
    DET_ID.with(|id| {
        *id.borrow_mut() = new_id.clone();
    });
}

#[cfg(test)]
mod tests {
    use std::thread::JoinHandle;
    use std::time::Duration;

    use rand::{thread_rng, Rng};
    use rusty_fork::rusty_fork_test;

    use crate::crossbeam_channel::unbounded;
    use crate::detthread::{generate_new_child_id, init_foreign_thread};
    use crate::test::set_rr_mode;
    use crate::{RRMode, Tivo};

    use super::{get_det_id, spawn};

    fn thread_id_assignment() {
        let _f = Tivo::init_tivo_thread_root_test();

        let h1 = spawn(move || {
            assert_eq!(&[1], get_det_id().as_slice());

            let h2 = spawn(move || {
                assert_eq!(&[1, 1], get_det_id().as_slice());

                let h3 = spawn(|| {
                    assert_eq!(&[1, 1, 1], get_det_id().as_slice());
                });
                let h4 = spawn(|| {
                    assert_eq!(&[1, 1, 2], get_det_id().as_slice());

                    let h5 = spawn(|| {
                        assert_eq!(&[1, 1, 2, 1], get_det_id().as_slice());
                    });
                    h5.join().unwrap();
                });

                h3.join().unwrap();
                h4.join().unwrap();
            });

            h2.join().unwrap();
        });
        h1.join().unwrap();
    }

    fn thread_id_assignment_random_times() {
        let _f = Tivo::init_tivo_thread_root_test();

        let mut v1: Vec<JoinHandle<_>> = vec![];
        let number_of_threads = thread_rng().gen_range(1, 15);

        for i in 1..=number_of_threads {
            let h1 = spawn(move || {
                // Wait a random amount of time, so that threads spawned after us have a chance
                // to spawn their children first.
                let n = thread_rng().gen_range(1, 15);
                std::thread::sleep(Duration::from_millis(n));

                let a = [i];
                assert_eq!(&a[..], get_det_id().as_slice());

                let mut v2 = vec![];
                // Threads use one-based indexing.
                for j in 1..=4 {
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
    }

    fn foreign_thread_spawn() {
        let child_id = generate_new_child_id();

        let h = std::thread::spawn(move || {
            init_foreign_thread(child_id);
        });

        if let Err(e) = h.join() {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    #[should_panic(expected = "thread not initialized")]
    fn foreign_thread_spawn_panic() {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);

        let h = std::thread::spawn(|| {
            let _chans = unbounded::<i32>();
        });

        if let Err(e) = h.join() {
            std::panic::resume_unwind(e);
        }
    }

    // "should_panic" tests don't work with rusty_fork. So we run these outside. It should be
    // okay...

    // Tivo::init_tivo_thread_root_test() should always be called before thread spawning.
    #[test]
    #[should_panic(expected = "thread not initialized")]
    fn failed_to_init_root() {
        spawn(|| {});
    }

    // Tivo::init_tivo_thread_root_test() should always be called before using the TLS get_det_id().
    #[test]
    #[should_panic(expected = "thread not initialized")]
    fn failed_to_init_root2() {
        get_det_id();
    }

    #[test]
    #[should_panic(expected = "Thread root already initialized!")]
    fn already_initialized() {
        Tivo::init_tivo_thread_root_test();
        Tivo::init_tivo_thread_root_test();
    }

    rusty_fork_test! {
    #[test]
    fn thread_id_assignment_test() {
        set_rr_mode(RRMode::NoRR);
        thread_id_assignment();
    }

    #[test]
    /// Create a thread tree and ensure the threads are given IDs according to their index.
    fn thread_id_assignment_random_times_test() {
        set_rr_mode(RRMode::NoRR);
        thread_id_assignment_random_times();
    }

    #[test]
    fn foreign_thread_spawn_test() {
        Tivo::init_tivo_thread_root_test();
        set_rr_mode(RRMode::NoRR);

        foreign_thread_spawn();
    }
    }
}
