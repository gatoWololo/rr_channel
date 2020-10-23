//The test creates two channels. The first channel is bounded with size 1. Channel 1 will
// continously return. Thread 1 because its capacity is full. Channel 2 will not return anything
// because it has unbounded capacity.
use rr_channel::detthread;

fn main() {
    let (s, r) = rr_channel::crossbeam_channel::bounded(1);
    let (s2, r2) = rr_channel::crossbeam_channel::unbounded();

    let h = detthread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s.send("Thread 1") {
                return;
            }
        }
    });

    let h2 = detthread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s2.send("Thread 2") {
                return;
            }
        }
    });

    for _ in 0..20 {
        rr_channel::select! {
            recv(r) -> x => println!("{:?}", x),
            recv(r2) -> y => println!("{:?}", y),
        }
    }

    // Drop receiver to allow bounded channel thread to exit.
    drop(r);
    drop(r2);

    h.join().unwrap();
    h2.join().unwrap();
}
