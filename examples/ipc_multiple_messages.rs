use rr_channel::ipc_channel;

fn main() {
    let (tx, rx) = ipc_channel::channel().unwrap();

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
