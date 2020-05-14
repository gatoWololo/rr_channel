use rr_channel::detthread;
use rr_channel::mpsc;

fn main() {
    let mut handles = Vec::new();

    // Create a shared channel that can be sent along from many threads.
    let (tx, rx) = mpsc::channel();
    for i in 0..10 {
        let tx = tx.clone();
        let h = detthread::spawn(move || {
            tx.send(i).unwrap();
        });
        handles.push(h);
    }

    for _ in 0..10 {
        let j = rx.recv().unwrap();
        println!("Received {}", j);
    }

    for h in handles {
        h.join().unwrap();
    }
}
