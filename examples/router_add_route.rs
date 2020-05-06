use rr_channel::ipc;
use rr_channel::router;
use rr_channel::router::ROUTER;
use rr_channel::crossbeam::Receiver;
/// Send messages and callbacks to be executed by the router thread.
use std::time::Duration;

fn main() -> Result<(), std::io::Error> {
    let (sender, receiver) = ipc::channel::<i32>()?;

    let f = Box::new(|result| println!("Result: {:?}", result));
    ROUTER.add_route(receiver, f);
    for i in 0..20 {
        sender.send(i);
    }

    // If you see router_add_route randomly failing, increase this number
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}
