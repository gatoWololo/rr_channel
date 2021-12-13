use rand::Rng;
use std::{thread, time};

use rr_channel::crossbeam_channel::Select;
use rr_channel::detthread;

fn main() {
    tracing_subscriber::fmt::Subscriber::builder()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_target(false)
            .without_time()
            .init();

    let tivo = rr_channel::Tivo::init_tivo_thread_root_test();
    let mut tx = Vec::new(); //transmiter
    let mut rx = Vec::new(); //receiver
    let mut join_handle_vec = Vec::new();
    //Creating Multiple Producers by Cloning the Transmitter
    for _ in 0..10
    //g tx and rx
    {
        let (transmiter, receiver) = rr_channel::crossbeam_channel::unbounded::<(i32, i32)>();
        //push and pop
        tx.push(transmiter); //Add an element to the end
        rx.push(receiver); //Add an element to the end
    }

    let _tx2 = tx.clone();
    //10 senders
    for a1 in 0..10 {
        // spawn 10 channels/txs(g)
        let sender = tx.pop().unwrap();
        //let sender = join_handle_vec.push().unwrap();
        //for a2 in 0..10{       //spwan
        //spawn to create a new thread and use move to move tx into the closure so the spawned thread owns tx
        let join_handle = detthread::spawn(move || {
            for a2 in 0..10
            //spwan
            {
                let delay = rand::thread_rng().gen_range(0..10);
                thread::sleep(time::Duration::from_millis(delay));
                sender.send((a1 as i32, a2)).unwrap();
            }
        });
        join_handle_vec.push(join_handle);
    }

    let mut sel = Select::new();
    //let mut opers = Vec::new();
    for i in 0..10 {
        sel.recv(&rx[i]);
    }
    // The second operation will be selected because it becomes ready first.

    for _ in 0..100 {
        let selected_operation = sel.select();
        let index = selected_operation.index();
        //selected_operation.recv(&rx[index]);
        println!("{:?}", selected_operation.recv(&rx[index]).unwrap());
    }
    for join_handle in join_handle_vec {
        join_handle
            .join()
            .expect("Couldn't join on the associated thread");
    }
    tivo.execution_done().unwrap();
}
