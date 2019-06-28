/// Test select::ready() function directly.
fn main() {
    let (s1, r1) = rr_channels::unbounded();
    let (s2, r2) = rr_channels::unbounded();

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

    let mut select = rr_channels::Select::new();
    select.recv(&r1);
    select.recv(&r2);
    for _ in 0..60 {
        println!("Index ready: {}", select.ready());
    }
}
