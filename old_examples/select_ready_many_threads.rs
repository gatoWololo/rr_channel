use rr_channel::detthread;

/// Spawn 10 channels.
/// We spawn 100 threads which share the sencer end of those 10 threads for a total
/// of 100 senders.
/// A single thread selects 1000 messages from the 10 receiver ends.

fn main() {
    rr_channel::init_tivo_thread_root();

    let mut receivers = Vec::new();
    let mut senders = Vec::new();

    for _ in 0..10 {
        let (s, r) = rr_channel::crossbeam_channel::unbounded();
        receivers.push(r);
        senders.push(s);
    }
    // Copy senders to avoid disconnecting.
    let _sender2 = senders.clone();

    for _ in 0..10 {
        let shared = senders.pop().unwrap();
        for _ in 0..10 {
            // Ten threads have the receiver end of this channel.
            let s2 = shared.clone();
            detthread::spawn(move || {
                for j in 0..10 {
                    s2.send(j).unwrap()
                }
            });
        }
    }

    let mut select = rr_channel::crossbeam_channel::Select::new();
    for r in receivers.iter() {
        select.recv(r);
    }

    for _ in 0..1000 {
        println!("Index ready: {}", select.ready());
    }
}
