use rr_channel::ipc;

fn main() {
    let (tx, rx) = ipc::channel().unwrap();
    let tx2 = tx.clone();

    rr_channel::detthread::spawn(move || {
        for i in 0..30 {
            tx.send(1).unwrap();
        }
    });

    rr_channel::detthread::spawn(move || {
        for i in 0..30 {
            tx2.send(2).unwrap();
        }
    });

    for i in 0..60 {
        let response = rx.recv().unwrap();
        println!("Thread {}", response);
    }
}
