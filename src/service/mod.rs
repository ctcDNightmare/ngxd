#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
fn execute(program: &str, args: &[&std::ffi::OsStr]) -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("Executing {program}"))?;
    ensure!(status.success(), "{program} exited with {status}");
    Ok(())
}

#[cfg(unix)]
pub use foreground as run;

pub fn foreground(path: &std::path::Path) -> anyhow::Result<()> {
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let signal = running.clone();
    ctrlc::set_handler(move || signal.store(false, std::sync::atomic::Ordering::Relaxed))?;
    crate::agent::run(path, running)
}
