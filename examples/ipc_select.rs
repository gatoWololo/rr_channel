use rand::Rng;
use rr_channel::ipc_channel;
use rr_channel::ipc_channel::IpcSelectionResult;
use std::time;

use rr_channel::detthread;

fn main() {
    let (s, r) = ipc_channel::channel::<u32>().unwrap();
    let (s2, r2) = ipc_channel::channel::<u32>().unwrap();
    let (s3, r3) = ipc_channel::channel::<u32>().unwrap();
    let mut set = ipc_channel::IpcReceiverSet::new().unwrap();

    detthread::spawn(move || loop {
        let delay = rand::thread_rng().gen_range(0, 3);
        std::thread::sleep(time::Duration::from_millis(delay));
        s.send(1);
    });
    detthread::spawn(move || loop {
        let delay = rand::thread_rng().gen_range(0, 5);
        std::thread::sleep(time::Duration::from_millis(delay));
        s2.send(2);
    });
    detthread::spawn(move || loop {
        let delay = rand::thread_rng().gen_range(0, 5);
        std::thread::sleep(time::Duration::from_millis(delay));
        s3.send(3);
    });

    set.add(r);
    set.add(r2);
    set.add(r3);

    for _ in 0..20 {
        println!("Select Results: ");
        for e in set.select().expect("Cannot select") {
            match e {
                IpcSelectionResult::ChannelClosed(index) => {
                    println!("IpcSelectionResult::ChannelClosed({})", index);
                }
                IpcSelectionResult::MessageReceived(index, opaque) => {
                    let val: u32 = opaque.to().unwrap();
                    println!("IpcSelectionResult::MessageReceived({}, {})", index, val);
                }
            }
        }
    }
}
