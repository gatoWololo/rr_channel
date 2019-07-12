use rr_channels::{after, unbounded};
/// Stolen from crossbeam_channel::after example!
use std::time::Duration;

fn main() {
    let (s, r) = unbounded::<i32>();
    let timeout = Duration::from_millis(100);

    rr_channels::select! {
        recv(r) -> msg => println!("received {:?}", msg),
        recv(after(timeout)) -> _ => println!("timed out"),
    }
}
