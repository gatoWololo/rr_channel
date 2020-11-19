//The test creates two channels. The first and the second rr channels are both waiting
//for the first event to occur, thus they are blocking the thread.

fn main() {
    rr_channel::init_tivo_thread_root();
    let (_, _) = rr_channel::crossbeam_channel::unbounded::<i32>();
    let (_, _) = rr_channel::crossbeam_channel::unbounded::<i32>();

    // detthread::spawn(move || {
    //     rr_channel::select! {
    //         recv(r) -> x => println!("channel r got value: {:?}", x),
    //         recv(r2) -> x => println!("channel r2 got value: {:?}", x),
    //     }
    //
    //     rr_channel::select! {
    //         recv(r) -> x => println!("channel r got value: {:?}", x),
    //         recv(r2) -> x => println!("channel r2 got value: {:?}", x),
    //     }
    // });
    //
    // s.send(3);
    // thread::sleep(Duration::from_millis(100));
}
