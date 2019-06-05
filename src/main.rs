use std::thread;
use crossbeam_channel::unbounded;
use crossbeam_channel::select;

fn main() {
    let (s1, r1) = unbounded();
    let (s2, r2) = unbounded();

    thread::spawn(move || {
        for _ in 0..30{
            s1.send(1).unwrap()
        }
    });
    thread::spawn(move || {
        for _ in 0..30{
            s2.send(2).unwrap()
        }
    });

    for _ in 0..60 {
        select! {
            recv(r1) -> _ => println!("receiver 1"),
            recv(r2) -> _ => println!("receiver 2"),
        }
    }
}
