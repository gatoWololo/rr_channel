use rr_channel::ipc_channel;
use rr_channel::ipc_channel::ipc;

fn main() {
    let (tx, rx) = ipc::channel().unwrap();

    rr_channel::detthread::spawn(move || {
        for i in 0..30 {
            tx.send(1).unwrap();
        }
    });

    for i in 0..30 {
        let response = rx.recv().unwrap();
        println!("Thread 1: {}", response);
    }
}
