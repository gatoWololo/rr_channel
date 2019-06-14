/// Send messages with the possibility to disconnect.
fn main() {
    let (s1, mut r1) = rr_channels::unbounded();
    let (s2, mut r2) = rr_channels::unbounded();

    rr_channels::spawn(move || {
        for _ in 0..30 {
            s1.send(1).unwrap()
        }
    });
    rr_channels::spawn(move || {
        for _ in 0..30 {
            s2.send(2).unwrap()
        }
    });

    for _ in 0..60 {
        rr_channels::rr_select! {
            recv(r1) -> x => println!("receiver 1: {:?}", x),
            recv(r2) -> y => println!("receiver 2: {:?}", y),
        }
    }
}
