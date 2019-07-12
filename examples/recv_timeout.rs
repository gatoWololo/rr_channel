use rand::Rng;
use rr_channels::thread;
use rr_channels::RecvTimeoutError;
use std::thread::sleep;
/// Thread write to the same sender with recv_timeout
/// Random delays to bring out more nondeterminism.
use std::time;

fn main() {
    let (s, r) = rr_channels::unbounded();
    // Avoid having channel disconnect.
    let _s = s.clone();

    thread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s.send("Thread 1") {
                return;
            }
            let delay = rand::thread_rng().gen_range(0, 40);
            sleep(time::Duration::from_millis(delay));
        }
    });

    // We may not read all 20 messages from sender. That's okay.
    for _ in 0..40 {
        // Delay, otherwise this thread burns through it's 40 iterations without
        // reading any messages.
        let delay = rand::thread_rng().gen_range(0, 20);
        match r.recv_timeout(time::Duration::from_millis(delay)) {
            Err(RecvTimeoutError::Timeout) => println!("Timeout"),
            Err(RecvTimeoutError::Disconnected) => println!("Disconnected"),
            Ok(msg) => println!("{}", msg),
        }
    }
}
