/// Two threads send their message through their own individual channels.
/// A 3rd thread creates a 4th thread which receives messages.
fn main() {
    let (s0, mut r0) = rr_channels::unbounded();
    let (s1, mut r1) = rr_channels::unbounded();

    let h0 = rr_channels::spawn(move || {
        for _ in 0..10 {
            if let Err(_) = s0.send(0) {
                return;
            }
        }
    });
    let h1 = rr_channels::spawn(move || {
        for _ in 0..10 {
            if let Err(_) = s1.send(1) {
                return;
            }
        }
    });

    let h2 = rr_channels::spawn(move || {
        // Spawn a secondary thread just for funsies.
        rr_channels::spawn(move || {
            for _ in 0..20 {
                rr_channels::rr_select! {
                    recv(r0) -> x => println!("receiver 0: {:?}", x),
                    recv(r1) -> y => println!("receiver 1: {:?}", y),
                }
            }
        }).join().expect("Couldn't wait on inner thread.");
    });

    h0.join().expect("Couldn't wait on thread 0");
    h1.join().expect("Couldn't wait on thread 1");
    h2.join().expect("Couldn't wait on thread 2");
}
