use std::thread;

/// Test select::ready() function directly.
fn main() {
    rr_channel::init_tivo_thread_root();

    let (s1, r1) = rr_channel::crossbeam_channel::unbounded();
    let (s2, r2) = rr_channel::crossbeam_channel::unbounded();

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

    let mut select = rr_channel::crossbeam_channel::Select::new();
    select.recv(&r1);
    select.recv(&r2);
    for _ in 0..60 {
        println!("Index ready: {}", select.ready());
    }
}
