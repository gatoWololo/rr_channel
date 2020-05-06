use rr_channel::ipc;
use rr_channel::router;
use rr_channel::router::ROUTER;
use rr_channel::crossbeam::Receiver;
/// Send messages to the router and have router send us back our messages
/// through callback with sender.
use std::time::Duration;

fn main() -> Result<(), std::io::Error> {
    let (sender, receiver) = ipc::channel::<i32>()?;
    let (sender2, receiver2) = ipc::channel::<i32>()?;

    let f = Box::new(move |result: Result<i32, _>| {
        sender2.send(result.unwrap());
    });
    ROUTER.add_route(receiver, f);
    for i in 0..20 {
        sender.send(i);
        println!("Result: {:?}", receiver2.recv());
    }

    // Needed, otherwise program might exit before router prints all messages.
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}
