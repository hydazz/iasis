use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use iasis::{
    agent::{Agent, Options},
    batch, parse_serial,
};

#[derive(Parser)]
#[command(
    version,
    about = "Attend every Iasis board: show each what its targets are doing, keep the restore images current and run the restores the boards and clients ask for"
)]
struct Arguments {
    /// Attend one board instead of every board
    #[arg(long, value_parser = parse_serial)]
    serial: Option<u64>,
    /// Restore with this idevicerestore instead of the host's engine, which
    /// is Apple Configurator where it is installed
    #[arg(long)]
    idevicerestore: Option<PathBuf>,
    /// Per-device worker logs and scratch directories; the cache directory
    /// by default
    #[arg(long)]
    logs: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "apple-silicon")]
    device: batch::Device,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let options = Options {
        serial: arguments.serial,
        idevicerestore: arguments.idevicerestore,
        logs: arguments.logs,
        family: arguments.device,
    };
    Agent::new(&options)?.run(&options)
}
