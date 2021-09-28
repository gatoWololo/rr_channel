//Minimal test case to show execution_done error when thread still sends message after calling execution_done.
//When we call execution done on the main thread it drops all the receivers
//Error: thread 'main' panicked at 'Unable to write to log: CannotWriteEventToLog("SendError(..)")', 
use rr_channel;

fn main(){    
let tivo = rr_channel::Tivo::init_tivo_thread_root_test();
let (sender, _) = rr_channel::crossbeam_channel::unbounded::<i32>();
tivo.execution_done().unwrap();
sender.send(1).unwrap();
}
