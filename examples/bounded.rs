use rr_channel::thread;

/// Two threads write to the same sender.
/// Channel two is never used.
fn main() {
    let (s, r) = rr_channel::bounded(1);
    let (s2, r2) = rr_channel::unbounded();

    thread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s.send("Thread 1") {
                return;
            }
        }
    });

    thread::spawn(move || {
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
}
