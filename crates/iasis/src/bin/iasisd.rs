use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use iasis::{
    agent::{Agent, Options},
    parse_serial,
};

#[derive(Parser)]
#[command(
    name = "iasisd",
    version,
    about = "Attend every Iasis board: show each what its targets are doing, keep the restore images current and run the restores the boards and clients ask for"
)]
struct Arguments {
    /// Attend one board instead of every board
    #[arg(long, value_parser = parse_serial)]
    serial: Option<u64>,
    /// Per-device worker logs and scratch directories; the cache directory
    /// by default
    #[arg(long)]
    logs: Option<PathBuf>,
}

fn main() -> Result<()> {
    iasis::native::run_if_worker();
    let arguments = Arguments::parse();
    let options = Options {
        serial: arguments.serial,
        logs: arguments.logs,
    };
    Agent::new(&options)?.run(&options)
}
