use rr_channel::thread;

/// Two threads write to the same sender.
/// Channel two is never used.
fn main() {
    let (s, r) = rr_channel::unbounded();
    // Avoid having channel disconnect.
    let _s = s.clone();

    let s1 = s.clone();
    thread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s1.send("Thread 1") {
                return;
            }
        }
    });

    let s2 = s.clone();
    thread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s2.send("Thread 2") {
                return;
            }
        }
    });

    for _ in 0..40 {
        let val = r.recv();
        println!("Got value: {:?}", val);
    }
}
