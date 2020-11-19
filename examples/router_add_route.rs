use rr_channel::ipc_channel;
use rr_channel::ipc_channel::router::ROUTER;
/// Send messages and callbacks to be executed by the router thread.
use std::time::Duration;

fn main() -> Result<(), std::io::Error> {
    rr_channel::init_tivo_thread_root();
    let (sender, receiver) = ipc_channel::ipc::channel::<i32>()?;

    let f = Box::new(|result| println!("Result: {:?}", result));
    ROUTER.add_route(receiver, f);
    for i in 0..20 {
        sender.send(i).expect("failed to send data");
    }

    // If you see router_add_route randomly failing, increase this number
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}
