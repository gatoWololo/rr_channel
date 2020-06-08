// use std::env;

// // Benchmarks the overhead of the different channel implementations (mpsc, ipc, crossbeam)

// // Multiple producers, single consumer. Requires waiting on the "right message"
// fn run_mpsc_experiments(chan: ChannelType) {
//     match chan {
//         ChannelType::Mpsc => {
//             mpsc_mpsc_experiment()
//         },
//         ChannelType::Ipc => {
//             mpsc_ipc_experiment()
//         },
//         ChannelType::Crossbeam => {
//             mpsc_crossbeam_experiment()
//         }
//     }
// }

// fn mpsc_mpsc_experiment {

// }

// fn mpsc_ipc_experiment {

// }

// fn mpsc_crossbeam_experiment {

// }

// /// Select a message from the "right" channel, one by one
// fn run_select_experiments(chan: ChannelType) {
//     match chan {
//         ChannelType::Mpsc => {
//             select_mpsc_experiment();
//         },
//         ChannelType::Ipc => {
//             select_ipc_experiment();
//         },
//         ChannelType::Crossbeam => {
//             select_crossbeam_experiment();
//         }
//     }
// }

// fn select_mpsc_experiment {

// }

// fn select_ipc_experiment {

// }

// fn select_crossbeam_experiment {

// }

// // MPSC + Select combined: send/recv messages across channel in a loop.
// fn run_combo_experiments(chan: ChannelType) {
// }

// fn combo_mpsc_experiment {

// }

// fn combo_ipc_experiment {

// }

// fn combo_crossbeam_experiment {

// }

// enum ChannelType {
//     Mpsc,
//     Crossbeam,
//     Ipc,
// }

// // Example input: cargo run --example -c mpsc
// fn main() {
//     let args : Vec<String> = env::args().collect();
//     print!("Args: {:?}", args);

//     let chan_type, experiment_type = match args.len() {
//         4 if args[2] == "-c" => {
//             ChannelType::Crossbeam, args[3]
//         },
//         4 if args[2] == "-i" => {
//             ChannelType::Ipc, args[3]
//         },
//         4 if args[2] == "-m" => {
//             ChannelType::Mpsc, args[3]
//         },
//         _ => {
//             ChannelType::Mpsc, String::from("combo")
//         },
//     };

//     // run mpsc, select, or combo experiment
//     match experiment_type {
//         "mpsc" => {
//             run_mpsc_experiments(chan_type);
//         },
//         "select" => {
//             run_select_experiments(chan_type);
//         },
//         "combo" => {
//             run_combo_experiments(chan_type);
//         },
//         // run combo option
//         _ => {
//             run_combo_experiments(chan_type);
//         }
//     }
// }

use std::env;
use std::process::Command;
use structopt::StructOpt;

#[derive(StructOpt)]
struct Benchmark {
    #[structopt(default_value = "crossbeam", long, short)]
    channel_type: String,
    #[structopt(default_value = "all", long, short)]
    experiment_type: String,
}

fn main() {
    let mut bench = Benchmark::from_args();

    let experiment_type = match bench.experiment_type.as_ref() {
        "mpsc" => String::from("mpsc"),
        "select" => String::from("select"),
        "combo" => String::from("combo"),
        _ => String::from("all"),
    };
    bench.experiment_type = experiment_type;

    let channel_type = match bench.channel_type.as_ref()
    {
        "mpsc" => String::from("mpsc"),
        "ipc" => String::from("ipc"),
        _ => String::from("crossbeam"),
    };
    bench.channel_type = channel_type;

    println!("CHANNEL_TYPE = {}", bench.channel_type);
    println!("EXPERIMENT_TYPE = {}", bench.experiment_type);
    
    Command::new("cargo bench")
        .env("CHANNEL_TYPE", bench.channel_type)
        .env("EXPERIMENT_TYPE", bench.experiment_type)
        .spawn() 
        .expect("Failed");
}
