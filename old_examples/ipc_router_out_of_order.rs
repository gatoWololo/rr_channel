use rand::Rng;
/// Originally I expected this to panic since receivers are being added
/// to a IpcReceiverSet (via ROUTER) in nondeterminism order. This is
/// something IpcReceiverSet checks for. But since the router uses an
/// IpcReceiverSet _itself_ as its main implementation, it means this is
/// automatically deterministic.
use rr_channel::ipc_channel;
use rr_channel::ipc_channel::router::ROUTER;
use std::time;

fn main() {
    rr_channel::init_tivo_thread_root();
    // To ensure deterministic router. We require the router to
    // be explicitly initialized at the very beginning!
    lazy_static::initialize(&ROUTER);
    let h1 = rr_channel::detthread::spawn(|| {
        add_receiver(1);
    });
    let h2 = rr_channel::detthread::spawn(|| {
        add_receiver(2);
    });
    let h3 = rr_channel::detthread::spawn(|| {
        add_receiver(3);
    });

    h1.join().expect("Couldn't wait on thread 1");
    h2.join().expect("Couldn't wait on thread 2");
    h3.join().expect("Couldn't wait on thread 3");
}

fn add_receiver(i: i32) {
    for _ in 1..i {
        let delay = rand::thread_rng().gen_range(0, 3);
        std::thread::sleep(time::Duration::from_millis(delay));

        let (_, r) = ipc_channel::ipc::channel::<u32>().unwrap();
        ROUTER.route_ipc_receiver_to_new_crossbeam_receiver(r);
    }
}
