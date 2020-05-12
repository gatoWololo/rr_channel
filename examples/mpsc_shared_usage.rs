use rr_channel::detthread;
use rr_channel::mpsc;

fn main() {
    // Create a shared channel that can be sent along from many threads
    // where tx is the sending half (tx for transmission), and rx is the receiving
    // half (rx for receiving).
    let (tx, rx) = mpsc::channel();
    for i in 0..10 {
        let tx = tx.clone();
        detthread::spawn(move || {
            tx.send(i).unwrap();
        });
    }

    for _ in 0..10 {
        let j = rx.recv().unwrap();
        assert!(0 <= j && j < 10);
    }
}
