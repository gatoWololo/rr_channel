use rand::Rng;
/// IpcReceiverSet internally checks that receivers are added in the same order,
/// with the same DetChannelId and index. This example should panic as receivers,
/// race on being added to the IpcReceiverSet.
use rr_channel::ipc;
use rr_channel::ipc::IpcReceiverSet;
use rr_channel::ipc::IpcSelectionResult;
use rr_channel::router::ROUTER;
use std::sync::{Arc, Mutex};
use std::time;

fn main() {
    let set = Arc::new(Mutex::new(IpcReceiverSet::new().expect("IpcReceiverSet")));
    let set2 = set.clone();
    let set3 = set.clone();

    let h1 = rr_channel::detthread::spawn(move || {
        add_receiver(1, set);
    });
    let h2 = rr_channel::detthread::spawn(|| {
        add_receiver(2, set2);
    });
    let h3 = rr_channel::detthread::spawn(|| {
        add_receiver(3, set3);
    });

    h1.join();
    h2.join();
    h3.join();
}

fn add_receiver(i: i32, set: Arc<Mutex<IpcReceiverSet>>) {
    for _ in 1..5 {
        let delay = rand::thread_rng().gen_range(0, 3);
        std::thread::sleep(time::Duration::from_millis(delay));

        let (_, r) = ipc::channel::<u32>().unwrap();
        set.lock().expect("Unable to acquire lock").add(r);
    }
}
