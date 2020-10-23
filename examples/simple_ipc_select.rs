use rr_channel::detthread;
use rr_channel::ipc_channel;
use rr_channel::ipc_channel::ipc::IpcSelectionResult;
use rr_channel::ipc_channel::ipc;

fn main() {
    let (s, r) = ipc_channel::ipc::channel::<u32>().unwrap();
    let (s2, r2) = ipc_channel::ipc::channel::<u32>().unwrap();
    let mut set = ipc::IpcReceiverSet::new().unwrap();

    detthread::spawn(move || {
        s.send(1);
    });
    detthread::spawn(move || {
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
