// Create a simple streaming channel - taken from mpsc example

use rr_channel::detthread;
use rr_channel::mpsc;

const EXPECTED_OUTPUT: &'static str = "./tests/expected_output.txt";
#[test]
fn main() {
    std::env::set_var("RR_MODE", "REPLAY");
    std::env::set_var("RR_RECORD_FILE", EXPECTED_OUTPUT);
    let tivo = rr_channel::Tivo::init_tivo_thread_root_test();
    let (tx, rx) = mpsc::channel();
    let h = detthread::spawn(move || {
        for _ in 0..1 {
            tx.send(10).unwrap();
        }
    });

    for _ in 0..1 {
        assert_eq!(rx.recv().unwrap(), 10);
    }

    h.join().unwrap();
    tivo.execution_done().unwrap();
}
