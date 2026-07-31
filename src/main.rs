use clap::Parser;
use tracing_subscriber::EnvFilter;

use box_proxy::{cli, config, record, replay};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Record(args) => {
            init_tracing(args.verbose);
            let cfg =
                config::record_config(args.port, args.dir, args.rewrite_on_record, args.config)?;
            record::run(cfg).await
        }
        cli::Command::Replay(args) => {
            init_tracing(args.verbose);
            let cfg = config::replay_config(args.port, args.dir, args.rewrite, args.absolute_base)?;
            replay::run(cfg).await
        }
    }
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let default = format!("box_proxy={},hyper=warn,hyper_util=warn,tower=warn", level);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&default));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
