use rr_channel::detthread;

/// Spawn 10 channels.
/// We spawn 100 threads which share the sencer end of those 10 threads for a total
/// of 100 senders.
/// A single thread selects 1000 messages from the 10 receiver ends.
use rr_channel;

fn main() {
    let mut receivers = Vec::new();
    let mut senders = Vec::new();

    for _ in 0..10 {
        let (s, r) = rr_channel::crossbeam::unbounded();
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

    for _ in 0..1000 {
        rr_channel::select! {
            recv(receivers[0]) -> x => println!("receiver 0: {:?}", x),
            recv(receivers[1]) -> y => println!("receiver 1: {:?}", y),
            recv(receivers[2]) -> y => println!("receiver 2: {:?}", y),
            recv(receivers[3]) -> y => println!("receiver 3: {:?}", y),
            recv(receivers[4]) -> y => println!("receiver 4: {:?}", y),
            recv(receivers[5]) -> y => println!("receiver 5: {:?}", y),
            recv(receivers[6]) -> y => println!("receiver 6: {:?}", y),
            recv(receivers[7]) -> y => println!("receiver 7: {:?}", y),
            recv(receivers[8]) -> y => println!("receiver 8: {:?}", y),
            recv(receivers[9]) -> y => println!("receiver 9: {:?}", y),
        }
    }
}
