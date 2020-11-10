//The test first creates a channel. It prints out "received {msg}" if the channel
//succussfully receives message, and print "time out" if the run is timed out. Because the channel wil
//not receive anything, the test always times out.

/// Stolen from crossbeam_channel::after example!
use std::time::Duration;
use rr_channel::crossbeam_channel::unbounded;
use rr_channel::crossbeam_channel::after;

fn main() {
    rr_channel::init_tivo_thread_root();
    let (s, r) = unbounded::<i32>();
    let timeout = Duration::from_millis(1);

    rr_channel::crossbeam_channel::select! {
        recv(r) -> msg => println!("received {:?}", msg),
        recv(after(timeout)) -> _ => println!("timed out"),
    }
}
