use rr_channel::ipc;
use rr_channel::ipc::IpcSelectionResult;

fn main() {
    let (s, r) = ipc::channel::<u32>().unwrap();
    let (s2, r2) = ipc::channel::<u32>().unwrap();
    let mut set = ipc::IpcReceiverSet::new().unwrap();

    rr_channel::thread::spawn(move || {
        s.send(1);
    });
    rr_channel::thread::spawn(move || {
        s2.send(2);
    });

    set.add(r);
    set.add(r2);

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
