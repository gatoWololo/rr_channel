use rr_channel::detthread;

/// Two threads write to the same sender.
/// Channel two is never used.
fn main() {
    rr_channel::init_tivo_thread_root();
    let (s, r) = rr_channel::crossbeam_channel::unbounded();
    // Avoid having channel disconnect.
    let _s = s.clone();

    let s1 = s.clone();
    detthread::spawn(move || {
        for _ in 0..20 {
            if s1.send("Thread 1").is_err() {
                return;
            }
        }
    });

    let s2 = s.clone();
    detthread::spawn(move || {
        for _ in 0..20 {
            if s2.send("Thread 2").is_err() {
                return;
            }
        }
    });

    for _ in 0..40 {
        let val = r.recv();
        println!("Got value: {:?}", val);
    }
}
