fn main() {
    let (s1, mut r1) = rr_channels::unbounded();
    let (s2, mut r2) = rr_channels::unbounded();

    // Keep copies around to avoid disconnecting channel.
    // Else we end up getting spurious RecvError from channel.
    let s1c = s1.clone();
    let s2c = s2.clone();

    rr_channels::spawn(move || {
        for _ in 0..30 {
            s1c.send(1).unwrap()
        }
    });
    rr_channels::spawn(move || {
        for _ in 0..30 {
            s2c.send(2).unwrap()
        }
    });

    for _ in 0..60 {
        rr_channels::rr_select! {
            recv(r1) -> x => println!("receiver 1: {:?}", x),
            recv(r2) -> y => println!("receiver 2: {:?}", y),
        }
    }
}
