use rr_channel::crossbeam_channel::Receiver;
use rr_channel::ipc_channel::ipc;
use rr_channel::ipc_channel::router::ROUTER;

fn main() -> Result<(), std::io::Error> {
    rr_channel::init_tivo_thread_root();

    let (sender, receiver) = ipc::channel()?;
    let sender2 = sender.clone();

    rr_channel::detthread::spawn(move || {
        for _ in 1..20 {
            sender2.send(2).expect("failed to send");
        }
    });

    rr_channel::detthread::spawn(move || {
        for _ in 1..20 {
            sender.send(1).expect("failed to send");
        }
    });

    let receiver: Receiver<i32> = ROUTER.route_ipc_receiver_to_new_crossbeam_receiver(receiver);

    for _ in 1..40 {
        match receiver.recv() {
            Ok(msg) => println!("Value: {}", msg),
            Err(e) => println!("Error when reading: {:?}", e),
        }
    }

    Ok(())
}
