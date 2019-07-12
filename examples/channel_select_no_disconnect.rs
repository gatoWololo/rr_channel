use rr_channel::thread;

fn main() {
    let (s1, r1) = rr_channel::unbounded();
    let (s2, r2) = rr_channel::unbounded();

    // Keep copies around to avoid disconnecting channel.
    // Else we end up getting spurious RecvError from channel.
    let s1c = s1.clone();
    let s2c = s2.clone();

    thread::spawn(move || {
        for _ in 0..30 {
            s1c.send(1).unwrap()
        }
    });
    thread::spawn(move || {
        for _ in 0..30 {
            s2c.send(2).unwrap()
        }
    });

    for _ in 0..60 {
        rr_channel::select! {
            recv(r1) -> x => println!("receiver 1: {:?}", x),
            recv(r2) -> y => println!("receiver 2: {:?}", y),
        }
    }
}
