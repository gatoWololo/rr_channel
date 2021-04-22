use rr_channel::ipc_channel::ipc;

fn main() {
    rr_channel::init_tivo_thread_root();
    let payload = "Hello, World!".to_owned();
    let (tx, rx) = ipc::channel().unwrap();

    // Send data
    tx.send(payload).unwrap();

    // Receive the data
    let response = rx.recv().unwrap();

    assert_eq!(response, "Hello, World!".to_owned());
}
