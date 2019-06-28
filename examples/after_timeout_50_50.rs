/// Times out roughly 50/50%
use std::{thread, time};
use rand::Rng;
use std::time::Duration;
use rr_channels::{after, unbounded};
use rand::thread_rng;

fn main() {
    let (s, r) = unbounded::<i32>();
    // Avoid having channel disconnect.
    let _s = s.clone();

    rr_channels::spawn(move || {
        // 50/50 chance of sleeping.
        if thread_rng().gen_bool(0.50) {
            thread::sleep(time::Duration::from_millis(150));
        }
        s.send(1);
    });

    let timeout = Duration::from_millis(50);
    rr_channels::select! {
        recv(r) -> msg => println!("received {:?}", msg),
        recv(after(timeout)) -> _ => println!("timed out"),
    }
}
