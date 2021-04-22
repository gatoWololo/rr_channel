//The channels will not disconnect because they are cloned.

use rr_channel::detthread;

fn main() {
    rr_channel::init_tivo_thread_root();
    let (s1, r1) = rr_channel::crossbeam_channel::unbounded();
    let (s2, r2) = rr_channel::crossbeam_channel::unbounded();

    // Keep copies around to avoid disconnecting channel.
    // Else we end up getting spurious RecvError from channel.
    let s1c = s1;
    let s2c = s2;

    detthread::spawn(move || {
        for _ in 0..30 {
            s1c.send(1).unwrap()
        }
    });
    detthread::spawn(move || {
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
