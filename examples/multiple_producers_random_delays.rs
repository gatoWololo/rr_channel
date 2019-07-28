use rand::Rng;
use rr_channel::thread;
/// Two threads write to the same sender.
/// Channel two is never used.
/// Random delays to bring out more nondeterminism.
use std::time;

fn main() {
    let (s, r) = rr_channel::unbounded();
    let (_s2, r2) = rr_channel::unbounded::<i32>();
    // Avoid having channel disconnect.
    let _s = s.clone();

    let s1 = s.clone();
    thread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s1.send("Thread 1") {
                return;
            }
            let delay = rand::thread_rng().gen_range(0, 30);
            thread::sleep(time::Duration::from_millis(delay));
        }
    });

    let s2 = s.clone();
    thread::spawn(move || {
        for _ in 0..20 {
            if let Err(_) = s2.send("Thread 2") {
                return;
            }
            let delay = rand::thread_rng().gen_range(0, 30);
            thread::sleep(time::Duration::from_millis(delay));
        }
    });

    for _ in 0..40 {
        rr_channel::select! {
            recv(r) -> x => println!("Got value: {:?}", x),
            recv(r2) -> x => println!("Got value: {:?}", x),
        }
    }
}
