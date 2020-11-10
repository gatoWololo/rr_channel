use rr_channel::ipc_channel;
use rr_channel::ipc_channel::router::ROUTER;
use rr_channel::ipc_channel::ipc;

fn main() -> Result<(), std::io::Error> {
    rr_channel::init_tivo_thread_root();

    // Send messages to ourself via the router.
    let (ipc_sender, ipc_receiver) = ipc::channel()?;
    let (sender, receiver) = rr_channel::crossbeam_channel::unbounded();
    ROUTER.route_ipc_receiver_to_crossbeam_sender(ipc_receiver, sender);
    for i in 1..20 {
        ipc_sender.send(i);
        println!("Saw value: {:?}", receiver.recv());
    }
    Ok(())
}
