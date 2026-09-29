use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = chromacon_style_manager::cli::Cli::parse();
    if let Err(err) = chromacon_style_manager::run(cli) {
        eprintln!("chromacon-style-manager: {err}");
        std::process::exit(1);
    }
    Ok(())
}
