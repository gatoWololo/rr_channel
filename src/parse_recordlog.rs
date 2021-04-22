use anyhow::Context;
use rr_channel::detthread::DetThreadId;
use rr_channel::recordlog::RecordEntry;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use structopt::StructOpt;

type Recordlog = HashMap<DetThreadId, VecDeque<RecordEntry>>;

#[derive(StructOpt, Debug)]
struct CmdLineOptions {
    #[structopt(parse(from_os_str))]
    recordlog_file: PathBuf,
    thread_id: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let opt = CmdLineOptions::from_args();

    let input = std::fs::read_to_string(&opt.recordlog_file)?;
    let recordlog: Recordlog = ron::from_str(&input)?;

    if let Some(thread_id) = opt.thread_id {
        let id: Vec<u32> = ron::from_str(&thread_id)?;
        let dti = DetThreadId::from(id.as_slice());
        let entries = recordlog
            .get(&dti)
            .with_context(|| format!("No such entry for thread: {:?}", dti))?;
        println!("{:#?}", entries);
    } else {
        println!("{:#?}", recordlog);
    }

    Ok(())
}
