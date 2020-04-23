// Create a simple streaming channel - taken from mpsc example

use rr_channel::mpsc;
use rr_channel::thread;

fn main() {
    let (tx, rx) = mpsc:: channel();
     thread::spawn(move|| {
     tx.send(10).unwrap();
     });
     
     assert_eq!(rx.recv().unwrap(), 10);
}
