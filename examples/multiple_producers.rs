use rr_channel::detthread;

/// Two threads write to the same sender.
/// Channel two is never used.
fn main() {
    rr_channel::init_tivo_thread_root();
    let (s, r) = rr_channel::crossbeam_channel::unbounded();
    let (_s2, r2) = rr_channel::crossbeam_channel::unbounded::<i32>();
    // Avoid having channel disconnect.
    let _s = s.clone();

    let s1 = s.clone();
    detthread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s1.send("Thread 1") {
                return;
            }
        }
    });

    let s2 = s.clone();
    detthread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s2.send("Thread 2") {
                return;
            }
        }
    });

    for _ in 0..40 {
        rr_channel::select! {
            recv(r) -> x => println!("Got value: {:?}", x),
            recv(r2) -> x => println!("Got value: {:?}", x),
        }
    }
}
