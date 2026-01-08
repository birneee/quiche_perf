use clap::{Parser, Subcommand};
use quiche_perf::args::{ClientArgs, ServerArgs};
use quiche_perf::client::client;
use quiche_perf::server::server;



#[derive(Subcommand)]
enum Commands {
    #[command(alias = "-c")]
    Client(ClientArgs),
    #[command(alias = "-s")]
    Server(ServerArgs)
}

#[derive(Parser)]
struct Args {
    #[clap(subcommand)]
    command: Commands
}


fn main() {
    env_logger::builder().format_timestamp_nanos().init();
    let args = Args::parse();

    match args.command {
        Commands::Client(args) => { client(&args); },
        Commands::Server(args) => server(&args, None),
    }
}
