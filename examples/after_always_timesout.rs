//The test first creates a channel. It prints out "received {msg}" if the channel
//succussfully receives message, and print "time out" if the run is timed out. Because the channel wil
//not receive anything, the test always times out.

use rr_channel::crossbeam_channel::after;
use rr_channel::crossbeam_channel::unbounded;
/// Stolen from crossbeam_channel::after example!
use std::time::Duration;

fn main() {
    rr_channel::init_tivo_thread_root();
    let (_, r) = unbounded::<i32>();
    let timeout = Duration::from_millis(1);

    rr_channel::crossbeam_channel::select! {
        recv(r) -> msg => println!("received {:?}", msg),
        recv(after(timeout)) -> _ => println!("timed out"),
    }
}
