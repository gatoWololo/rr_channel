use rr_channel::detthread;
use rr_channel::crossbeam;
use std::sync::Mutex;
use std::sync::Arc;
use std::env;


fn main() {
    let mut senders: Vec<Mutex<_>> = Vec::new();
    let mut receivers: Vec<Mutex<_>> = Vec::new();
    let args: Vec<String> = env::args().collect();
    let size = args[1].clone().parse().unwrap();

    for _ in 0..size {
        let (s, r) = crossbeam::unbounded::<i32>();
        senders.push(Mutex::new(s));
        receivers.push(Mutex::new(r));
    }
    
    let senders = Arc::new(senders);
    let receivers = Arc::new(receivers);

    let mut handles = Vec::new();

    //Set up the ring
    let sender = senders[0].try_lock().expect("Lock already taken");
    sender.send(1);

    for i in 0..size {
        // Clone via arc for moving into closure.
        let senders = senders.clone();
        let receivers = receivers.clone();
        let h = rr_channel::detthread::spawn(move || {

            // Locks should never block as every thread accesses exclusive members of vector.
            let sender = senders[(i + 1) % size].try_lock().expect("Lock already taken");
            let receiver = receivers[(i) % size].try_lock().expect("Lock already taken");
            // Send message around ring.
            let result = receiver.recv().expect("Appropriate error");
            println!("{}",result);
            if result == 1{
                println!("Hi");
                sender.send(1 /*token*/);
            }
        });
        handles.push(h);
    }

    for h in handles {
        h.join().unwrap();
    }
}