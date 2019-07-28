use rr_channel::ipc;
use rr_channel::ipc::IpcSelectionResult;
use rr_channel::router::ROUTER;
use rand::Rng;
use std::time;

fn main() {
    // To ensure deterministic router. We require the router to
    // be explicitly initialized at the very beginning!
    lazy_static::initialize(&ROUTER);
    let h1 = rr_channel::thread::spawn(|| {
        add_receiver(1);
    });
    let h2 = rr_channel::thread::spawn(||  {
        add_receiver(2);
    });
    let h3 = rr_channel::thread::spawn(||  {
        add_receiver(3);
    });

    h1.join();
    h2.join();
    h3.join();
}

fn add_receiver(i: i32) {
    for _ in 1..5 {
        let delay = rand::thread_rng().gen_range(0, 3);
        std::thread::sleep(time::Duration::from_millis(delay));

        let (_, r) = ipc::channel::<u32>().unwrap();
        ROUTER.route_ipc_receiver_to_new_crossbeam_receiver(r);
    }
}
