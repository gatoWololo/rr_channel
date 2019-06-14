/// Two threads write to the same sender.
/// Channel two is never used.
/// Random delays to bring out more nondeterminism.
use std::thread::JoinHandle;
use std::{thread, time};
use rand::Rng;

fn main() {
    let (mut s, mut r) = rr_channels::unbounded();
    let (mut _s2, mut r2) = rr_channels::unbounded::<i32>();
    // Avoid hanving channel disconnect.
    let s_dup = s.clone();

    let s1 = s.clone();
    rr_channels::spawn(move || {
        for i in 0..20 {
            s1.send("Thread 1");
            let delay = rand::thread_rng().gen_range(0, 30);
            thread::sleep(time::Duration::from_millis(delay));
        }
    });

    let s2 = s.clone();
    rr_channels::spawn(move || {
        for i in 0..20 {
            s2.send("Thread 2");
            let delay = rand::thread_rng().gen_range(0, 30);
            thread::sleep(time::Duration::from_millis(delay));
        }
    });

    for i in 0..40 {
        rr_channels::rr_select! {
            recv(r) -> x => println!("Got value: {:?}", x),
            recv(r2) -> x => println!("Got value: {:?}", x),
        }
    }
}
