use rand::thread_rng;
use rand::Rng;
use rr_channel::detthread;
use std::thread::sleep;
/// Stolen from crossbeam_channel::never example.
/// Modified to 50/50 timeout.
/// Expected to panic once in a while due to non-determinisim we cannot
/// handle but catch when replaying.
use std::time::Duration;

fn main() {
    rr_channel::init_tivo_thread_root();

    let (s, r) = rr_channel::crossbeam_channel::unbounded();

    detthread::spawn(move || {
        sleep(Duration::from_secs(1));
        s.send(1).unwrap();
    });

    // Suppose this duration can be a `Some` or a `None`.
    let duration = if thread_rng().gen_bool(0.50) {
        Some(Duration::from_millis(100))
    } else {
        None
    };

    // Create a channel that times out after the specified duration.
    let timeout = duration
        .map(rr_channel::crossbeam_channel::after)
        .unwrap_or_else(rr_channel::crossbeam_channel::never);

    rr_channel::select! {
        recv(r) -> msg => println!("Message: {:?}", msg),
        recv(timeout) -> _ => println!("timed out"),
    }
}
