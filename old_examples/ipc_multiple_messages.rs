use rr_channel::ipc_channel::ipc;

fn main() {
    rr_channel::init_tivo_thread_root();
    let (tx, rx) = ipc::channel().unwrap();

    rr_channel::detthread::spawn(move || {
        for _ in 0..30 {
            tx.send(1).unwrap();
        }
    });

    for _ in 0..30 {
        let response = rx.recv().unwrap();
        println!("Thread 1: {}", response);
    }
}
