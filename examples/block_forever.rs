//The test creates two channels. The first and the second rr channels are both waiting 
//for the first event to occur, thus they are blocking the thread.

use rr_channel;
use rr_channel::thread;
use std::time::Duration;

fn main() {
    let (s, r) = rr_channel::unbounded::<i32>();
    let (s2, r2) = rr_channel::unbounded::<i32>();

    thread::spawn(move || {
        rr_channel::select! {
            recv(r) -> x => println!("Got value: {:?}", x),
            recv(r2) -> x => println!("Got value: {:?}", x),
        }

        rr_channel::select! {
            recv(r) -> x => println!("Got value: {:?}", x),
            recv(r2) -> x => println!("Got value: {:?}", x),
        }
    });

    s.send(3);
    thread::sleep(Duration::from_millis(30));
}
