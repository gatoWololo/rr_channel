use rr_channel::ipc_channel;

fn main() {
    let (tx, rx) = ipc_channel::channel().unwrap();
    let tx2 = tx.clone();
    let tx3 = tx.clone();
    let tx4 = tx.clone();

    spawn_sender(1, tx);
    spawn_sender(2, tx2);
    spawn_sender(3, tx3);
    spawn_sender(4, tx4);

    for i in 0..120 {
        let response = rx.recv().unwrap();
        println!("Thread {}", response);
    }
}

fn spawn_sender(v: i32, tx: ipc_channel::IpcSender<i32>) {
    rr_channel::detthread::spawn(move || {
        for i in 0..30 {
            tx.send(v).unwrap();
        }
    });
}
