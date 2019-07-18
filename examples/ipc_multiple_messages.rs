use rr_channel::ipc;

fn main() {
    let (tx, rx) = ipc::channel().unwrap();

    rr_channel::thread::spawn(move || {
        for i in 0..30 {
            tx.send(1).unwrap();
        }
    });

    for i in 0..30 {
        let response = rx.recv().unwrap();
        println!("Thread 1: {}", response);
    }
}
