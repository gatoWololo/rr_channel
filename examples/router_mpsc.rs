use rr_channel::ipc;
use rr_channel::router;
use rr_channel::router::ROUTER;
use rr_channel::Receiver;

fn main() -> Result<(), std::io::Error> {
    let (sender, receiver) = ipc::channel()?;
    let sender2 = sender.clone();

    rr_channel::thread::spawn(move || {
        for _ in 1..20 {
            sender2.send(2);
        }
    });

    rr_channel::thread::spawn(move || {
        for _ in 1..20 {
            sender.send(1);
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
