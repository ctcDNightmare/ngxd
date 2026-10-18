use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};
const LABEL: &str = "dev.ngxd.agent";
fn plist_path() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("Cannot determine home directory")?
        .home_dir()
        .join("Library/LaunchAgents/dev.ngxd.agent.plist"))
}
fn domain() -> Result<String> {
    let output = std::process::Command::new("id").arg("-u").output()?;
    ensure!(output.status.success(), "Cannot determine uid");
    Ok(format!("gui/{}", String::from_utf8(output.stdout)?.trim()))
}
fn xml(path: &Path) -> Result<String> {
    let value = path.to_str().context("Service paths must be UTF-8")?;
    ensure!(!value.chars().any(char::is_control), "Invalid service path");
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}
pub fn install(config: &Path) -> Result<()> {
    let path = plist_path()?;
    ensure!(
        !path.exists(),
        "Service already installed; uninstall it first"
    );
    let log = directories::BaseDirs::new()
        .context("Cannot determine home")?
        .home_dir()
        .join("Library/Logs/ngxd.log");
    fs::create_dir_all(path.parent().unwrap())?;
    fs::create_dir_all(log.parent().unwrap())?;
    fs::write(
        &path,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{}</string><string>--config</string><string>{}</string><string>run</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ThrottleInterval</key><integer>5</integer>
<key>ExitTimeOut</key><integer>150</integer>
<key>StandardOutPath</key><string>{}</string>
<key>StandardErrorPath</key><string>{}</string>
</dict></plist>
"#,
            xml(&std::env::current_exe()?)?,
            xml(config)?,
            xml(&log)?,
            xml(&log)?
        ),
    )?;
    println!(
        "Installed {}. Run `ngxd start` after configuring folder and api_key.",
        path.display()
    );
    Ok(())
}
pub fn control(action: &str) -> Result<()> {
    let domain = domain()?;
    let target = format!("{domain}/{LABEL}");
    match action {
        "start" => super::execute(
            "launchctl",
            &[
                OsStr::new("bootstrap"),
                OsStr::new(&domain),
                plist_path()?.as_os_str(),
            ],
        ),
        "stop" => super::execute("launchctl", &[OsStr::new("bootout"), OsStr::new(&target)]),
        "status" => super::execute("launchctl", &[OsStr::new("print"), OsStr::new(&target)]),
        _ => anyhow::bail!("Unsupported action"),
    }
}
pub fn uninstall() -> Result<()> {
    let target = format!("{}/{LABEL}", domain()?);
    let loaded = std::process::Command::new("launchctl")
        .args(["print", &target])
        .output()?;
    if loaded.status.success() {
        control("stop")?;
    }
    fs::remove_file(plist_path()?)?;
    Ok(())
}
