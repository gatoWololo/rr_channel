use rr_channel::detthread;
use rr_channel::crossbeam;
use std::sync::Mutex;
use std::sync::Arc;

fn main() {
    let mut senders: Vec<Mutex<_>> = Vec::new();
    let mut receivers: Vec<Mutex<_>> = Vec::new();

    for _ in 0..10 {
        let (s, r) = crossbeam::unbounded::<i32>();
        senders.push(Mutex::new(s));
        receivers.push(Mutex::new(r));
    }
    
    let senders = Arc::new(senders);
    let receivers = Arc::new(receivers);

    for i in 0..10 {
        // Clone via arc for moving into closure.
        let senders = senders.clone();
        let receivers = receivers.clone();
        rr_channel::detthread::spawn(move || {
            //Set up the ring
            if i == 0 {
                let sender = senders[0].try_lock().expect("Lock already taken");
                sender.send(1);
            }

            // Locks should never block as every thread accesses exclusive members of vector.
            let sender = senders[(i + 1) % 10].try_lock().expect("Lock already taken");
            let receiver = receivers[(i) % 10].try_lock().expect("Lock already taken");
            // Send message around ring.
            let result = receiver.try_recv().unwrap();
            println!("{}",result);
            if result == 1{
                println!("Hi");
                sender.send(1 /*token*/);
            }
        });
    }
}