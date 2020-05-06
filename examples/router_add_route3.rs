/// Two threads race sending messages to router. Router
/// forwards messages via a custom callback.
/// Main thread prints messages. Definetily nondeterministic.
/// Deterministic thanks to our RR B)
use rand::Rng;
use rr_channel::ipc;
use rr_channel::router;
use rr_channel::router::ROUTER;
use rr_channel::crossbeam::Receiver;
use std::time;
use std::time::Duration;
fn main() -> Result<(), std::io::Error> {
    let (sender, receiver) = ipc::channel::<i32>()?;
    let (sender2, receiver2) = ipc::channel::<i32>()?;
    let thread_sender1 = sender.clone();
    let thread_sender2 = sender.clone();

    let f = Box::new(move |result: Result<i32, _>| {
        sender2.send(result.unwrap());
    });
    ROUTER.add_route(receiver, f);

    let h1 = rr_channel::detthread::spawn(move || {
        for i in 0..20 {
            let delay = rand::thread_rng().gen_range(0, 5);
            std::thread::sleep(time::Duration::from_millis(delay));
            thread_sender1.send(1);
        }
    });
    let h2 = rr_channel::detthread::spawn(move || {
        for i in 0..20 {
            let delay = rand::thread_rng().gen_range(0, 5);
            std::thread::sleep(time::Duration::from_millis(delay));
            thread_sender2.send(2);
        }
    });

    for i in 0..20 {
        println!("Result: {:?}", receiver2.recv());
    }

    h1.join();
    h2.join();
    Ok(())
}
