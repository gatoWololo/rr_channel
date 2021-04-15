// Create a simple streaming channel - taken from mpsc example

use rr_channel::detthread;
use rr_channel::mpsc;
use rr_channel::recordlog::GlobalReplayer;

const OUTPUT: &'static str = "./tests/generated_output.txt";
#[test]
fn main() {
    std::env::set_var("RR_MODE", "Record");
    std::env::set_var("RR_RECORD_FILE", OUTPUT);
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

    let g1 = GlobalReplayer::new(OUTPUT);
    let g2 = GlobalReplayer::new("./tests/expected_output.txt");
    assert_eq!(g1, g2);
}
