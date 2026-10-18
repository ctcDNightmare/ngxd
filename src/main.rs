mod agent;
mod config;
mod paperless;
mod queue;
mod service;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Absolute or relative config path; defaults to the user's config directory.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run in the foreground (also used by Unix service managers).
    Run,
    /// Create a default config without overwriting an existing one.
    Init,
    /// Print the resolved config path.
    ConfigPath,
    /// Register this executable with the OS service manager; does not start it.
    Install,
    /// Retry failed uploads/processing jobs (stop the daemon first).
    RetryFailed,
    /// Start the installed service.
    Start,
    /// Stop the installed service.
    Stop,
    /// Show service status.
    Status,
    /// Remove the service registration; keep config and queued jobs.
    Uninstall,
    #[command(hide = true)]
    ServiceRun,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = config::resolve(cli.config)?;
    match cli.command {
        Command::ConfigPath => println!("{}", path.display()),
        Command::Init => {
            config::load_or_create(&path)?;
            println!("Config ready: {}", path.display());
        }
        Command::Run => service::foreground(&path)?,
        Command::ServiceRun => service::run(&path)?,
        Command::Install => {
            config::load_or_create(&path)?;
            service::install(&path)?;
        }
        Command::RetryFailed => agent::retry_failed(&path)?,
        Command::Start => service::control("start")?,
        Command::Stop => service::control("stop")?,
        Command::Status => service::control("status")?,
        Command::Uninstall => service::uninstall()?,
    }
    Ok(())
}
