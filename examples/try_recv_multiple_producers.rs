/// Multiple threads write to the same sender with try_recv()
/// Random delays to bring out more nondeterminism.
use std::{thread, time};
use rand::Rng;
use rr_channels::TryRecvError;

fn main() {
    let (s, r) = rr_channels::unbounded();
    let s2 = s.clone();
    // Avoid having channel disconnect.
    let _s = s.clone();

    rr_channels::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s.send("Thread 1") {
                return;
            }
            let delay = rand::thread_rng().gen_range(0, 40);
            thread::sleep(time::Duration::from_millis(delay));
        }
    });

    rr_channels::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s2.send("Thread 2") {
                return;
            }
            let delay = rand::thread_rng().gen_range(0, 40);
            thread::sleep(time::Duration::from_millis(delay));
        }
    });

    // We may not read all 40 messages from senders. That's okay.
    for _ in 0..60 {
        // Delay, otherwise this thread burns through it's 60 iterations without
        // reading any messages. We add a lower delay number to intermix thread1,
        // thread2 and empty messages.
        let delay = rand::thread_rng().gen_range(0, 15);
        thread::sleep(time::Duration::from_millis(delay));
        match r.try_recv() {
            Err(TryRecvError::Empty) => println!("Empty"),
            Err(TryRecvError::Disconnected) => println!("Disconnected"),
            Ok(msg) => println!("{}", msg),
        }
    }
}