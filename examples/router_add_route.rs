use rr_channel::crossbeam_channel::Receiver;
use rr_channel::ipc_channel;
use rr_channel::router;
use rr_channel::router::ROUTER;
/// Send messages and callbacks to be executed by the router thread.
use std::time::Duration;

fn main() -> Result<(), std::io::Error> {
    let (sender, receiver) = ipc_channel::channel::<i32>()?;

    let f = Box::new(|result| println!("Result: {:?}", result));
    ROUTER.add_route(receiver, f);
    for i in 0..20 {
        sender.send(i);
    }

    // If you see router_add_route randomly failing, increase this number
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}
