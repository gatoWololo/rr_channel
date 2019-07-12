use rand::thread_rng;
use rand::Rng;
use rr_channel::{after, thread, unbounded};
use std::thread::sleep;
/// Times out roughly 50/50%
use std::time;
use std::time::Duration;

fn main() {
    let (s, r) = unbounded::<i32>();
    // Avoid having channel disconnect.
    let _s = s.clone();

    thread::spawn(move || {
        // 50/50 chance of sleeping.
        if thread_rng().gen_bool(0.50) {
            sleep(time::Duration::from_millis(150));
        }
        s.send(1);
    });

    let timeout = Duration::from_millis(50);
    rr_channel::select! {
        recv(r) -> msg => println!("received {:?}", msg),
        recv(after(timeout)) -> _ => println!("timed out"),
    }
}
