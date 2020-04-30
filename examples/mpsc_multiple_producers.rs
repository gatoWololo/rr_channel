use rr_channel::{mpsc, thread};

/// Send messages with the possibility to disconnect.
fn main() {
    let (s1, r1) = mpsc::sync_channel(35);
    let (s2, r2) = mpsc::sync_channel(35);

    println!("{:?}", thread::get_det_id());

    thread::spawn(move || {
        for _ in 0..30 {
            println!("{:?}", thread::get_det_id());
            s1.send(1).unwrap()
        }
    });
    thread::spawn(move || {
        println!("{:?}", thread::get_det_id());
        for _ in 0..30 {
            s2.send(2).unwrap()
        }
    });

    for _ in 0..60 {
        if let Ok(res) = r1.recv() {
            println!("Received message s1 -> r1: {:?}", res);
        }
        if let Ok(res) = r2.recv() {
            println!("Received message s2 -> r2: {:?}", res);
        }
    }
}
