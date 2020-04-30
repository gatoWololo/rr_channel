//The test first creates a channel. It prints out "received {msg}" if the channel
//succussfully receives message, and print "time out" if the run is timed out. Because the channel wil
//not receive anything, the test always times out.

use rr_channel::{after, unbounded};
/// Stolen from crossbeam_channel::after example!
use std::time::Duration;

fn main() {
    let (s, r) = unbounded::<i32>();
    let timeout = Duration::from_millis(100);

    rr_channel::select! {
        recv(r) -> msg => println!("received {:?}", msg),
        recv(after(timeout)) -> _ => println!("timed out"),
    }
}
