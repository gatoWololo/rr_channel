use rr_channel::crossbeam_channel::Receiver;
use rr_channel::ipc_channel;
/// Receiver messages from ourselves via the router, from different receivers
/// each time.
use rr_channel::ipc_channel::{router, ipc};
use rr_channel::ipc_channel::router::ROUTER;

fn main() -> Result<(), std::io::Error> {
    rr_channel::init_tivo_thread_root();

    for i in 0..20 {
        let (sender, receiver) = ipc::channel()?;

        let receiver: Receiver<i32> = ROUTER.route_ipc_receiver_to_new_crossbeam_receiver(receiver);
        sender.send(i);
        sender.send(i + 100);
        println!("Value: {}", receiver.recv().expect("Cannot read message"));
    }

    Ok(())
}
