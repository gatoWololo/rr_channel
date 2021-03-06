use rr_channel::mpsc;

fn main() {
    rr_channel::init_tivo_thread_root();
    let (tx, rx) = mpsc::channel();

    rr_channel::detthread::spawn(move || {
        for _ in 0..30 {
            tx.send(1).unwrap();
        }
    });

    for _ in 0..30 {
        let response = rx.recv().unwrap();
        println!("Thread 1: {}", response);
    }
}
