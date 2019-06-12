use crossbeam_channel::unbounded;

fn main() {
    let mut receivers = Vec::new();
    for i in 0..10 {
        let (s, r) = unbounded();
        receivers.push(r);

        rr_channels::spawn(move || {
            for _ in 0..10 {
                s.send(i).unwrap()
            }
        });
    }

    for _ in 0..100 {
        rr_channels::rr_select! {
            recv(receivers[0]) -> x => println!("receiver 0: {:?}", x),
            recv(receivers[1]) -> y => println!("receiver 1: {:?}", y),
            recv(receivers[2]) -> y => println!("receiver 2: {:?}", y),
            recv(receivers[3]) -> y => println!("receiver 3: {:?}", y),
            recv(receivers[4]) -> y => println!("receiver 4: {:?}", y),
            recv(receivers[5]) -> y => println!("receiver 5: {:?}", y),
            recv(receivers[6]) -> y => println!("receiver 6: {:?}", y),
            recv(receivers[7]) -> y => println!("receiver 7: {:?}", y),
            recv(receivers[8]) -> y => println!("receiver 8: {:?}", y),
            recv(receivers[9]) -> y => println!("receiver 9: {:?}", y),
        }
    }
}
