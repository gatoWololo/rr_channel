//The test first creates a channel. Then, there is 50/50 chance that the channel
//sleeps, which will cause the test to time out at the end.

use rand::thread_rng;
use rand::Rng;
use rr_channel::crossbeam_channel::{after, unbounded};
use rr_channel::detthread;
use std::thread::sleep;
/// Times out roughly 50/50%
use std::time;
use std::time::Duration;

fn main() {
    rr_channel::init_tivo_thread_root();
    let (s, r) = unbounded::<i32>();
    // Avoid having channel disconnect.
    let _s = s.clone();

    detthread::spawn(move || {
        // 50/50 chance of sleeping.
        if thread_rng().gen_bool(0.50) {
            sleep(time::Duration::from_millis(10));
        }
        s.send(1).expect("failed to send message");
    });

    let timeout = Duration::from_millis(50);
    rr_channel::select! {
        recv(r) -> msg => println!("received {:?}", msg),
        recv(after(timeout)) -> _ => println!("timed out"),
    }
}
