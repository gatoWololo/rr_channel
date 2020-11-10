use rr_channel::{detthread, mpsc};

// Omar: TODO IDK if this tests as much, since the messages are always present
// by the time the main thread starts doing .recv() the messages will always
// arrive in order.
fn main() {
    rr_channel::init_tivo_thread_root();
    let (s1, r1) = mpsc::sync_channel(35);
    let (s2, r2) = mpsc::sync_channel(35);

    println!("{:?}", detthread::get_det_id());

    detthread::spawn(move || {
        for _ in 0..30 {
            // println!("{:?}", detthread::get_det_id());
            s1.send(1).unwrap()
        }
    });
    detthread::spawn(move || {
        // println!("{:?}", detthread::get_det_id());
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
