use rr_channel::crossbeam;
use std::sync::Mutex;
use std::sync::Arc;
use std::env;

const USAGE: &str = "Expected cmdline argument [Number of threads] [Loop Iterations]";

fn main() {
    let mut senders: Vec<Mutex<_>> = Vec::new();
    let mut receivers: Vec<Mutex<_>> = Vec::new();
    let args: Vec<String> = env::args().collect();

    let size  = args.get(1).expect(USAGE).parse().expect(USAGE);
    let loop_times = args.get(2).expect(USAGE).parse().expect(USAGE);

    for _ in 0..size {
        let (s, r) = crossbeam::unbounded::<i32>();
        senders.push(Mutex::new(s));
        receivers.push(Mutex::new(r));
    }

    let senders = Arc::new(senders);
    let receivers = Arc::new(receivers);

    //Set up the ring
    senders[0].try_lock().expect("Lock already taken").send(1).expect("Cannot send message");
    let mut handles = vec![];

    for i in 0..size {
        // Clone via arc for moving into closure.
        let senders = senders.clone();
        let receivers = receivers.clone();

        let h = rr_channel::detthread::spawn(move || {
            // Locks should never block as every thread accesses exclusive members of vector.
            let sender = senders[(i + 1) % size].try_lock().expect("Lock already taken");
            let receiver = receivers[(i) % size].try_lock().expect("Lock already taken");

            for _ in 0..loop_times {
                let result = receiver.recv().expect("Cannot recv message");
                println!("Received {}", result);
                sender.send(result).expect("Cannot send message");
            }

        });
        handles.push(h);
    }

    for h in handles {
        h.join().unwrap();
    }
}
