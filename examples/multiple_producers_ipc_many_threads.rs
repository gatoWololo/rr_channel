use rr_channel::ipc_channel;
use rr_channel::ipc_channel::ipc;

fn main() {
    rr_channel::init_tivo_thread_root();
    let (tx, rx) = ipc::channel().unwrap();
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

fn spawn_sender(v: i32, tx: ipc_channel::ipc::IpcSender<i32>) {
    rr_channel::detthread::spawn(move || {
        for i in 0..30 {
            tx.send(v).unwrap();
        }
    });
}
