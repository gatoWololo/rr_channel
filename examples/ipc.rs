use rand::Rng;
use rr_channel::ipc_channel::ipc;
use rr_channel::ipc_channel::ipc::IpcReceiverSet;
use rr_channel::ipc_channel::ipc::IpcSelectionResult;
use std::{thread, time};

fn main() {
    let tivo = rr_channel::Tivo::init_tivo_thread_root_test();

    let mut tx = Vec::new(); //transmiter
    let mut rx_set = IpcReceiverSet::new().unwrap();
    let mut join_handle_vec = Vec::new();

    //Creating Multiple Producers by Cloning the Transmitter
    for _ in 0..10 {
        let (transmiter, receiver) = ipc::channel::<(i32, i32)>().unwrap();
        tx.push(transmiter); //Add an element to the end
        rx_set.add(receiver).unwrap(); //Collection of IpcReceivers moved into the set;
                                       //thus creating a common (and exclusive) endpoint
                                       //for receiving messages on any of the added channels.
    }

    let _tx2 = tx.clone();

    for a1 in 0..10 {
        let sender = tx.pop().unwrap();
        let join_handle = rr_channel::detthread::spawn(move || {
            for a2 in 0..10 {
                let delay = rand::thread_rng().gen_range(0..10);
                thread::sleep(time::Duration::from_millis(delay));
                sender.send((a1 as i32, a2)).unwrap();
            }
        });
        join_handle_vec.push(join_handle);
    }
    /*
    for _ in 0..100{
        for e in rx_set.select().expect("Cannot select") {
            match e {
                IpcSelectionResult::ChannelClosed(index) => {
                    println!("IpcSelectionResult::ChannelClosed({})", index);
                }
                IpcSelectionResult::MessageReceived(index, opaque) => {
                    let val: u32 = opaque.to().unwrap();
                    println!("IpcSelectionResult::MessageReceived({}, {})", index, val);
                }
            }
        }
    }
    */
    let mut count = 0;
    for _ in 0..100 {
        if count == 100 
        {
            break;
        }
                // Wait for events to come from our select() new channels are added to
        // our ReceiverSet below
        // Poll the receiver set for any readable events
        else{
            for receiver in rx_set.select().unwrap() {
                match receiver {
                    IpcSelectionResult::MessageReceived(_msg_id, msg) => {
                        let val: (i32, i32) = msg.to().unwrap();
                        println!("{:?}", val);
                    }
                    IpcSelectionResult::ChannelClosed(msg_id) => {
                        println!("No more data from {}", msg_id);
                    }
                }
                count = count+1;
            }
        }
  
    }

    for join_handle in join_handle_vec {
        join_handle
            .join()
            .expect("Couldn't join on the associated thread");
    }

    tivo.execution_done().unwrap();
}
