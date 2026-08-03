mod cli;
mod commands;
mod context;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    if let Err(err) = commands::run(cli.command) {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}
