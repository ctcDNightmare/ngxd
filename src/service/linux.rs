use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

fn unit_path() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("Cannot determine config directory")?;
    Ok(base.config_dir().join("systemd/user/ngxd.service"))
}

// systemd performs percent specifier and dollar environment expansion even without a shell.
fn quote(path: &Path) -> Result<String> {
    let value = path.to_str().context("Service paths must be UTF-8")?;
    ensure!(!value.contains(['\n', '\r', '\0']), "Invalid service path");
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}

pub fn install(config: &Path) -> Result<()> {
    let path = unit_path()?;
    ensure!(
        !path.exists(),
        "Service already installed; uninstall it first"
    );
    let exe = std::env::current_exe()?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(
        &path,
        format!(
            "[Unit]\nDescription=ngxd folder watcher\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={} --config {} run\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec=150\n\n[Install]\nWantedBy=default.target\n",
            quote(&exe)?,
            quote(config)?
        ),
    )?;
    super::execute(
        "systemctl",
        &[OsStr::new("--user"), OsStr::new("daemon-reload")],
    )?;
    super::execute(
        "systemctl",
        &[
            OsStr::new("--user"),
            OsStr::new("enable"),
            OsStr::new("ngxd.service"),
        ],
    )?;
    println!(
        "Installed {}. Run `ngxd start` after configuring folder and api_key.",
        path.display()
    );
    Ok(())
}

pub fn control(action: &str) -> Result<()> {
    super::execute(
        "systemctl",
        &[
            OsStr::new("--user"),
            OsStr::new(action),
            OsStr::new("ngxd.service"),
        ],
    )
}

pub fn uninstall() -> Result<()> {
    super::execute(
        "systemctl",
        &[
            OsStr::new("--user"),
            OsStr::new("disable"),
            OsStr::new("--now"),
            OsStr::new("ngxd.service"),
        ],
    )?;
    fs::remove_file(unit_path()?)?;
    super::execute(
        "systemctl",
        &[OsStr::new("--user"), OsStr::new("daemon-reload")],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_systemd_expansions() {
        assert_eq!(
            quote(Path::new("/tmp/a % $ \" b")).unwrap(),
            "\"/tmp/a %% $$ \\\" b\""
        );
        assert!(quote(Path::new("/tmp/a\nb")).is_err());
    }
}
