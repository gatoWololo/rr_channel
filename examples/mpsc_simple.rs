// Create a simple streaming channel - taken from mpsc example

use rr_channel::mpsc;
use rr_channel::detthread;

fn main() {
    let (tx, rx) = mpsc::channel();
    detthread::spawn(move || {
        tx.send(10).unwrap();
    });

    assert_eq!(rx.recv().unwrap(), 10);
}
