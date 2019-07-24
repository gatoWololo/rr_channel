use rr_channel::router;
use rr_channel::router::ROUTER;
use rr_channel::ipc;
use rr_channel::Receiver;

fn main() -> Result<(), std::io::Error>{
    let (sender, receiver) = ipc::channel()?;

    let receiver: Receiver<i32> =
        ROUTER.route_ipc_receiver_to_new_crossbeam_receiver(receiver);
    sender.send(200);

    println!("Value: {}", receiver.recv().expect("Cannot read message"));

    Ok(())
}
