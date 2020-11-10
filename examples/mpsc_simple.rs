// Create a simple streaming channel - taken from mpsc example

use rr_channel::detthread;
use rr_channel::mpsc;

fn main() {
    rr_channel::init_tivo_thread_root();
    let (tx, rx) = mpsc::channel();
    detthread::spawn(move || {
        tx.send(10).unwrap();
    });

    assert_eq!(rx.recv().unwrap(), 10);
}
