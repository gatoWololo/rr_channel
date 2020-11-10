use rr_channel::detthread;

/// Two threads send their message through their own individual channels.
/// A 3rd thread creates a 4th thread which receives messages.
fn main() {
    rr_channel::init_tivo_thread_root();

    let (s0, r0) = rr_channel::crossbeam_channel::unbounded();
    let (s1, r1) = rr_channel::crossbeam_channel::unbounded();

    let h0 = detthread::spawn(move || {
        for _ in 0..10 {
            if let Err(_) = s0.send(0) {
                return;
            }
        }
    });
    let h1 = detthread::spawn(move || {
        for _ in 0..10 {
            if let Err(_) = s1.send(1) {
                return;
            }
        }
    });

    let h2 = detthread::spawn(move || {
        // Spawn a secondary thread just for funsies.
        detthread::spawn(move || {
            for _ in 0..20 {
                rr_channel::select! {
                    recv(r0) -> x => println!("receiver 0: {:?}", x),
                    recv(r1) -> y => println!("receiver 1: {:?}", y),
                }
            }
        })
        .join()
        .expect("Couldn't wait on inner thread.");
    });

    h0.join().expect("Couldn't wait on thread 0");
    h1.join().expect("Couldn't wait on thread 1");
    h2.join().expect("Couldn't wait on thread 2");
}
