use rr_channels::thread;
use rr_channels;
use std::time::Duration;

fn main() {
    let (s, r) = rr_channels::unbounded::<i32>();
    let (s2, r2) = rr_channels::unbounded::<i32>();

    thread::spawn(move || {
        rr_channels::select! {
            recv(r) -> x => println!("Got value: {:?}", x),
            recv(r2) -> x => println!("Got value: {:?}", x),
        }

        rr_channels::select! {
            recv(r) -> x => println!("Got value: {:?}", x),
            recv(r2) -> x => println!("Got value: {:?}", x),
        }
    });

    s.send(3);
    thread::sleep(Duration::from_millis(30));
}
