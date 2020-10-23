use rr_channel::ipc_channel;

fn main() {
    let payload = "Hello, World!".to_owned();
    let (tx, rx) = ipc_channel::channel().unwrap();

    // Send data
    tx.send(payload).unwrap();

    // Receive the data
    let response = rx.recv().unwrap();

    assert_eq!(response, "Hello, World!".to_owned());
}
