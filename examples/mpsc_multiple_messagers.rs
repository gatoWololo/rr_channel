use rr_channel::detthread;
use rr_channel::mpsc;

fn main() {
    let (tx, rx) = mpsc::channel();

    rr_channel::detthread::spawn(move || {
        for i in 0..30 {
            tx.send(1).unwrap();
        }
    });

    for i in 0..30 {
        let response = rx.recv().unwrap();
        println!("Thread 1: {}", response);
    }
}
