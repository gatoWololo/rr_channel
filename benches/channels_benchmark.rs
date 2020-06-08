use criterion::{criterion_group, criterion_main, Criterion};
use std::env;
use std::sync::Mutex;
use std::sync::Arc;
use rr_channel::{detthread, mpsc, ipc, crossbeam, RECORD_MODE};
use std::sync::mpsc as orig_mpsc;
use ipc_channel::ipc as orig_ipc;
use crossbeam_channel as orig_crossbeam;

#[derive(Copy, Clone)]
pub enum ChannelType {
    Mpsc,
    Crossbeam,
    Ipc,
}

#[derive(Copy, Clone)]
pub enum BenchmarkCase {
    Record,
    Replay,
    PassThrough,
    Baseline,
}

#[inline]
fn set_record_mode_var(ben: BenchmarkCase) {
    match ben {
        BenchmarkCase::Baseline => {},
        BenchmarkCase::PassThrough => {
            env::set_var("RR_CHANNEL", "noRR");
        },
        BenchmarkCase::Record => {
            env::set_var("RR_CHANNEL", "record");
        },
        BenchmarkCase::Replay => {
            env::set_var("RR_CHANNEL", "replay");
        },
    }
    lazy_static::initialize(&RECORD_MODE);
}

#[inline]
fn run_all_experiments(chan: ChannelType, ben: BenchmarkCase) {
    match chan {
        ChannelType::Mpsc => {
            // ring_mpsc_experiment(ben)
        },
        ChannelType::Ipc => {
            mpsc_ipc_experiment(ben);
            select_ipc_experiment(ben);
            ring_ipc_experiment(ben);
        },
        ChannelType::Crossbeam => {
            mpsc_crossbeam_experiment(ben);
            select_crossbeam_experiment(ben);
            ring_crossbeam_experiment(ben);
        },
    }
}

#[inline]
fn run_ring_experiments(chan: ChannelType, ben: BenchmarkCase) {
    match chan {
        ChannelType::Mpsc => ring_mpsc_experiment(ben),
        ChannelType::Ipc => ring_ipc_experiment(ben),
        ChannelType::Crossbeam => ring_crossbeam_experiment(ben),
    }
}

#[inline]
fn ring_mpsc_experiment(ben: BenchmarkCase) {    
    match ben {
        BenchmarkCase::Baseline => {
            let mut senders: Vec<Mutex<_>> = Vec::new();
            let mut receivers: Vec<Mutex<_>> = Vec::new();

            for _ in 0..NUM_ITERATIONS {
                let (s, r) = orig_mpsc::channel();
                senders.push(Mutex::new(s));
                receivers.push(Mutex::new(r));
            }
        
            let senders = Arc::new(senders);
            let receivers = Arc::new(receivers);
            for i in 0..NUM_THREADS {
                // Clone via arc for moving into closure.
                let senders = senders.clone();
                let receivers = receivers.clone();
                detthread::spawn(move || {
                    // Locks should never block as every thread accesses exclusive members of vector.
                    let sender = senders[(i + 1) % NUM_THREADS].try_lock().expect("Lock already taken");
                    let _receiver = receivers[i].try_lock().expect("Lock already taken");
                    // Send message around ring.
                    sender.send(1 /*token*/).unwrap();
                });
            }
        },
        _ => {
            let mut senders: Vec<Mutex<_>> = Vec::new();
            let mut receivers: Vec<Mutex<_>> = Vec::new();

            for _ in 0..NUM_ITERATIONS {
                let (s, r) = mpsc::channel();
                senders.push(Mutex::new(s));
                receivers.push(Mutex::new(r));
            }
        
            let senders = Arc::new(senders);
            let receivers = Arc::new(receivers);
            for i in 0..NUM_THREADS {
                // Clone via arc for moving into closure.
                let senders = senders.clone();
                let receivers = receivers.clone();
                detthread::spawn(move || {
                    // Locks should never block as every thread accesses exclusive members of vector.
                    let sender = senders[(i + 1) % NUM_THREADS].try_lock().expect("Lock already taken");
                    let _receiver = receivers[i].try_lock().expect("Lock already taken");
                    // Send message around ring.
                    sender.send(1 /*token*/).unwrap();
                });
            }
        },
    }
}

#[inline]
fn ring_ipc_experiment(ben: BenchmarkCase) {
    match ben {
        BenchmarkCase::Baseline => {
            let mut senders: Vec<Mutex<_>> = Vec::new();
            let mut receivers: Vec<Mutex<_>> = Vec::new();

            for _ in 0..NUM_ITERATIONS {
                let (s, r) = orig_ipc::channel().unwrap();
                senders.push(Mutex::new(s));
                receivers.push(Mutex::new(r));
            }
        
            let senders = Arc::new(senders);
            let receivers = Arc::new(receivers);
            for i in 0..NUM_THREADS {
                // Clone via arc for moving into closure.
                let senders = senders.clone();
                let receivers = receivers.clone();
                detthread::spawn(move || {
                    // Locks should never block as every thread accesses exclusive members of vector.
                    let sender = senders[(i + 1) % NUM_THREADS].try_lock().expect("Lock already taken");
                    let _receiver = receivers[i].try_lock().expect("Lock already taken");
                    // Send message around ring.
                    sender.send(1 /*token*/).unwrap();
                });
            }
        },
        _ => {
            let mut senders: Vec<Mutex<_>> = Vec::new();
            let mut receivers: Vec<Mutex<_>> = Vec::new();

            for _ in 0..NUM_ITERATIONS {
                let (s, r) = ipc::channel().unwrap();
                senders.push(Mutex::new(s));
                receivers.push(Mutex::new(r));
            }
        
            let senders = Arc::new(senders);
            let receivers = Arc::new(receivers);
            for i in 0..NUM_THREADS {
                // Clone via arc for moving into closure.
                let senders = senders.clone();
                let receivers = receivers.clone();
                detthread::spawn(move || {
                    // Locks should never block as every thread accesses exclusive members of vector.
                    let sender = senders[(i + 1) % NUM_THREADS].try_lock().expect("Lock already taken");
                    let _receiver = receivers[i].try_lock().expect("Lock already taken");
                    // Send message around ring.
                    sender.send(1 /*token*/).unwrap();
                });
            }
        },
    }
}

#[inline]
fn ring_crossbeam_experiment(ben: BenchmarkCase) {
    match ben {
        BenchmarkCase::Baseline => {
            let mut senders: Vec<Mutex<_>> = Vec::new();
            let mut receivers: Vec<Mutex<_>> = Vec::new();

            for _ in 0..NUM_ITERATIONS {
                let (s, r) = orig_crossbeam::unbounded::<i32>();
                senders.push(Mutex::new(s));
                receivers.push(Mutex::new(r));
            }
        
            let senders = Arc::new(senders);
            let receivers = Arc::new(receivers);
            for i in 0..NUM_THREADS {
                // Clone via arc for moving into closure.
                let senders = senders.clone();
                let receivers = receivers.clone();
                detthread::spawn(move || {
                    // Locks should never block as every thread accesses exclusive members of vector.
                    let sender = senders[(i + 1) % NUM_THREADS].try_lock().expect("Lock already taken");
                    let _receiver = receivers[i].try_lock().expect("Lock already taken");
                    // Send message around ring.
                    sender.send(1 /*token*/).unwrap();
                });
            }
        },
        _ => {
            let mut senders: Vec<Mutex<_>> = Vec::new();
            let mut receivers: Vec<Mutex<_>> = Vec::new();

            for _ in 0..NUM_ITERATIONS {
                let (s, r) = crossbeam::unbounded::<i32>();
                senders.push(Mutex::new(s));
                receivers.push(Mutex::new(r));
            }
        
            let senders = Arc::new(senders);
            let receivers = Arc::new(receivers);
            for i in 0..NUM_THREADS {
                // Clone via arc for moving into closure.
                let senders = senders.clone();
                let receivers = receivers.clone();
                detthread::spawn(move || {
                    // Locks should never block as every thread accesses exclusive members of vector.
                    let sender = senders[(i + 1) % NUM_THREADS].try_lock().expect("Lock already taken");
                    let _receiver = receivers[i].try_lock().expect("Lock already taken");
                    // Send message around ring.
                    sender.send(1 /*token*/).unwrap();
                });
            }
        },
    }
}

// Multiple producers, single consumer. Requires waiting on the "right message"
#[inline]
fn run_mpsc_experiments(chan: ChannelType, ben: BenchmarkCase) {
    match chan {
        ChannelType::Mpsc => mpsc_mpsc_experiment(ben),
        ChannelType::Ipc => mpsc_ipc_experiment(ben),
        ChannelType::Crossbeam => mpsc_crossbeam_experiment(ben),
    }
}

#[inline]
fn mpsc_mpsc_experiment(ben: BenchmarkCase) {
    match ben {
        BenchmarkCase::Baseline => {
            let (s1, r) = orig_mpsc::channel();
        
            for _ in 0..NUM_THREADS {
                let s_new = s1.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                r.recv().unwrap();
            }
        },
        _ => {
            let (s1, r) = mpsc::channel();
            
            for _ in 0..NUM_THREADS {
                let s_new = s1.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                r.recv().unwrap();
            }

        },
    }
}

#[inline]
fn mpsc_ipc_experiment(ben: BenchmarkCase) {
    match ben {
        BenchmarkCase::Baseline => {
            let (s1, r) = orig_ipc::channel().unwrap();

            for _ in 0..NUM_THREADS {
                let s_new = s1.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                r.recv().unwrap();
            }
        },
        _ => {
            let (s1, r) = ipc::channel().unwrap();
            
            for _ in 0..NUM_THREADS {
                let s_new = s1.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                r.recv().unwrap();
            }
        }
    }

}

#[inline]
fn mpsc_crossbeam_experiment(ben: BenchmarkCase) {
    match ben {
        BenchmarkCase::Baseline => {
            let (s1, r) = orig_crossbeam::unbounded();

            for _ in 0..NUM_THREADS {
                let s_new = s1.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                r.recv().unwrap();
            }
        },
        _ => {
            let (s1, r) = crossbeam::unbounded();
            
            for _ in 0..NUM_THREADS {
                let s_new = s1.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                r.recv().unwrap();
            }
        }
    }
}

/// Select a message from the "right" channel, one by one
#[inline]
fn run_select_experiments(chan: ChannelType, ben: BenchmarkCase) {
    match chan {
        ChannelType::Mpsc => {
            // select_mpsc_experiment(ben);
            println!("")
        }
        ChannelType::Ipc => {
            select_ipc_experiment(ben);
        }
        ChannelType::Crossbeam => {
            select_crossbeam_experiment(ben);
        }
    }
}

// #[inline]
// fn select_mpsc_experiment(ben: BenchmarkCase) {
//     match ben {
//         BenchmarkCase::Baseline => {
//         },
//         _ => {

//         },
// }
#[inline]
fn select_ipc_experiment(ben: BenchmarkCase) {
    match ben {
        BenchmarkCase::Baseline => {
            let (s, r) = orig_ipc::channel::<u32>().unwrap();
            let mut set = orig_ipc::IpcReceiverSet::new().unwrap();

            for _ in 0..NUM_THREADS {
                let s_new = s.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                r.recv().unwrap();
            }

            set.add(r).unwrap();

            for e in set.select().expect("Cannot select") {
                match e {
                    orig_ipc::IpcSelectionResult::ChannelClosed(_index) => {
                        // println!("IpcSelectionResult::ChannelClosed({})", index);
                    }
                    orig_ipc::IpcSelectionResult::MessageReceived(_index, _opaque) => {
                        // let val: u32 = opaque.to().unwrap();
                        // println!("IpcSelectionResult::MessageReceived({}, {})", index, val);
                    }
                }
            }
        },
        _ => {
            let (s, r) = ipc::channel::<u32>().unwrap();
            let mut set = ipc::IpcReceiverSet::new().unwrap();

            for _ in 0..NUM_THREADS {
                let s_new = s.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                r.recv().unwrap();
            }

            set.add(r).unwrap();

            for e in set.select().expect("Cannot select") {
                match e {
                    ipc::IpcSelectionResult::ChannelClosed(_index) => {}
                    ipc::IpcSelectionResult::MessageReceived(_index, _opaque) => {}
                }
            }
        },
    }
}

#[inline]
fn select_crossbeam_experiment(ben: BenchmarkCase) {
    match ben {
        BenchmarkCase::Baseline => {
            let (s1, r) = orig_crossbeam::unbounded();

            for _ in 0..NUM_THREADS {
                let s_new = s1.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }
        
            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                orig_crossbeam::select! {
                    recv(r) -> _x => {},
                }
            }
            
        },
        _ => {
            let (s1, r) = crossbeam::unbounded();

            for _ in 0..NUM_THREADS {
                let s_new = s1.clone();
                detthread::spawn(move || {
                    for _ in 0..NUM_ITERATIONS {
                        s_new.send(1).unwrap()
                    }
                });
            }

            for _ in 0..(NUM_ITERATIONS*NUM_THREADS) {
                rr_channel::select! {
                    recv(r) -> _x => {},
                }
            }
        },
    }
}

// MPSC + Select combined: send/recv messages across channel in a loop.
#[inline]
fn run_combo_experiments(chan: ChannelType, ben: BenchmarkCase) {
    match chan {
        ChannelType::Mpsc => {
            // select_mpsc_experiment(ben);
        }
        ChannelType::Ipc => {
            ring_ipc_experiment(ben);
            select_ipc_experiment(ben);
        }
        ChannelType::Crossbeam => {
            ring_ipc_experiment(ben);
            select_crossbeam_experiment(ben);
        }
    }
}

// Example input: cargo run --example -c mpsc
/// Benchmarks the overhead of the different channel implementations (mpsc, ipc, crossbeam)
#[inline]
fn channels_benchmark(c: &mut Criterion) {
    // let mut bench = Benchmark::from_args();

    // let experiment_type = match bench.experiment_type.as_ref() {
    //     "mpsc" => String::from("mpsc"),
    //     "select" => String::from("select"),
    //     "combo" => String::from("combo"),
    //     _ => String::from("all"),
    // };
    // bench.experiment_type = experiment_type;

    // let channel_type = match bench.channel_type.as_ref()
    // {
    //     "mpsc" => String::from("mpsc"),
    //     "ipc" => String::from("ipc"),
    //     _ => String::from("crossbeam"),
    // };
    // bench.channel_type = channel_type;

    // println!("CHANNEL_TYPE = {}", bench.channel_type);
    // println!("EXPERIMENT_TYPE = {}", bench.experiment_type);
    // env::set_var("CHANNEL_TYPE", bench.channel_type);
    // env::set_var("EXPERIMENT_TYPE", bench.experiment_type);

    let _crossbeam_var = "crossbeam".to_owned();
    let _ipc_var = "ipc".to_owned();
    let _mpsc_var = "mpsc".to_owned();

    let experiment_type = match env::var("EXPERIMENT_TYPE") {
        Ok(exp) => exp,
        _ => String::from("all"),
    };

    let chan_type = match env::var("CHANNEL_TYPE") {
        Ok(var) => {
            match var.as_ref() {
                "crossbeam" => ChannelType::Crossbeam,
                "ipc" => ChannelType::Ipc,
                _ => ChannelType::Mpsc,
            }
        },
        _ => ChannelType::Crossbeam,
    };

    let mut group = c.benchmark_group("Channel Benchmark");

    // run mpsc, select, or combo experiment
    match experiment_type.as_str() {
        "mpsc" => {
            set_record_mode_var(BenchmarkCase::PassThrough);
            group.bench_function("run_mpsc_experiments (pass-through)", 
            |b| {
                b.iter(|| run_mpsc_experiments(chan_type,
                BenchmarkCase::PassThrough))
            });

            set_record_mode_var(BenchmarkCase::Record);
            group.bench_function("run_mpsc_experiments (record)", 
            |b| {
                b.iter(|| run_mpsc_experiments(chan_type,
                BenchmarkCase::Record))
            });

            set_record_mode_var(BenchmarkCase::Replay);
            group.bench_function("run_mpsc_experiments (replay)", 
            |b| {
                b.iter(|| run_mpsc_experiments(chan_type,
                BenchmarkCase::Replay))
            });

            // baseline
            group.bench_function("run_mpsc_experiments (baseline)", 
            |b| {
                b.iter(|| run_mpsc_experiments(chan_type, 
                BenchmarkCase::Baseline))
            });

        }
        "select" => {
            set_record_mode_var(BenchmarkCase::PassThrough);
            group.bench_function("run_select_experiments", |b| {
                b.iter(|| run_select_experiments(chan_type,
                BenchmarkCase::PassThrough))
            });

            set_record_mode_var(BenchmarkCase::Record);
            group.bench_function("run_select_experiments", |b| {
                b.iter(|| run_select_experiments(chan_type,
                BenchmarkCase::Record))
            });

            set_record_mode_var(BenchmarkCase::Replay);
            group.bench_function("run_select_experiments", |b| {
                b.iter(|| run_select_experiments(chan_type,
                BenchmarkCase::Replay))
            });

            group.bench_function("run_select_experiments", |b| {
                b.iter(|| run_select_experiments(chan_type,
                BenchmarkCase::Baseline))
            });

        } 
        "combo" => {
            set_record_mode_var(BenchmarkCase::PassThrough);
            group.bench_function("run_combo_experiments", |b| {
                b.iter(|| run_combo_experiments(chan_type,
                BenchmarkCase::PassThrough))
            });

            set_record_mode_var(BenchmarkCase::Record);
            group.bench_function("run_combo_experiments", |b| {
                b.iter(|| run_combo_experiments(chan_type,
                BenchmarkCase::Record))
            });

            set_record_mode_var(BenchmarkCase::Replay);
            group.bench_function("run_combo_experiments", |b| {
                b.iter(|| run_combo_experiments(chan_type,
                BenchmarkCase::Replay))
            });

            group.bench_function("run_combo_experiments", |b| {
                b.iter(|| run_combo_experiments(chan_type,
                BenchmarkCase::Baseline))
            });
        }
        "ring" => {
            set_record_mode_var(BenchmarkCase::PassThrough);
            group.bench_function("run_ring_experiments", |b| {
                b.iter(|| run_ring_experiments(chan_type,
                BenchmarkCase::PassThrough))
            });

            set_record_mode_var(BenchmarkCase::Record);
            group.bench_function("run_ring_experiments", |b| {
                b.iter(|| run_ring_experiments(chan_type,
                BenchmarkCase::Record))
            });

            set_record_mode_var(BenchmarkCase::Replay);
            group.bench_function("run_ring_experiments", |b| {
                b.iter(|| run_ring_experiments(chan_type,
                BenchmarkCase::Replay))
            });

            group.bench_function("run_ring_experiments", |b| {
                b.iter(|| run_ring_experiments(chan_type,
                BenchmarkCase::Baseline))
            });
        },
        _ => {
            set_record_mode_var(BenchmarkCase::PassThrough);
            group.bench_function("run_all_experiments", |b| {
                b.iter(|| run_all_experiments(chan_type,
                BenchmarkCase::PassThrough))
            });

            set_record_mode_var(BenchmarkCase::Record);
            group.bench_function("run_all_experiments", |b| {
                b.iter(|| run_all_experiments(chan_type,
                BenchmarkCase::Record))
            });

            set_record_mode_var(BenchmarkCase::Replay);
            group.bench_function("run_all_experiments", |b| {
                b.iter(|| run_all_experiments(chan_type,
                BenchmarkCase::Replay))
            });

            group.bench_function("run_all_experiments", |b| {
                b.iter(|| run_all_experiments(chan_type,
                BenchmarkCase::Baseline))
            });
        },

    }

    group.finish();
}

use structopt::StructOpt;

#[derive(StructOpt)]
struct Benchmark {
    #[structopt(default_value = "crossbeam", long, short)]
    channel_type: String,
    #[structopt(default_value = "all", long, short)]
    experiment_type: String,
}

static NUM_THREADS: usize = 30;
static NUM_ITERATIONS: usize = 30;

criterion_group!(benches, channels_benchmark);
criterion_main!(benches);
