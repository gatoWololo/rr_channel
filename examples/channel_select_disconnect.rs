//The test creates two channels that send messages with the possibility to disconnect
//because they are not cloned.

use rr_channel::detthread;

/// Send messages with the possibility to disconnect.
fn main() {
    let (s1, r1) = rr_channel::crossbeam_channel::unbounded();
    let (s2, r2) = rr_channel::crossbeam_channel::unbounded();

    detthread::spawn(move || {
        for _ in 0..30 {
            s1.send(1).unwrap()
        }
    });
    detthread::spawn(move || {
        for _ in 0..30 {
            s2.send(2).unwrap()
        }
    });

    for _ in 0..60 {
        rr_channel::select! {
            recv(r1) -> x => println!("receiver 1: {:?}", x),
            recv(r2) -> y => println!("receiver 2: {:?}", y),
        }
    }
}
