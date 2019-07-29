use rr_channel::ipc;
use rr_channel::router;
use rr_channel::router::ROUTER;
use rr_channel::Receiver;
/// Send messages and callbacks to be executed by the router thread.
use std::time::Duration;

fn main() -> Result<(), std::io::Error> {
    let (sender, receiver) = ipc::channel::<i32>()?;

    let f = Box::new(|result| println!("Result: {:?}", result));
    ROUTER.add_route(receiver, f);
    for i in 0..20 {
        sender.send(i);
    }

    // Needed, otherwise program might exit before router prints all messages.
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}
