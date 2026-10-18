use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};
const NAME: &str = "ngxd";
define_windows_service!(ffi_service_main, service_main);

pub fn run(_: &Path) -> Result<()> {
    service_dispatcher::start(NAME, ffi_service_main)?;
    Ok(())
}
fn service_main(_: Vec<OsString>) {
    let _ = service_body();
}
fn service_body() -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();
    let handle = service_control_handler::register(NAME, move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            signal.store(false, Ordering::Relaxed);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    let status = |state, code| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    handle.set_service_status(status(ServiceState::Running, 0))?;
    let result = (|| {
        let cli = <crate::Cli as clap::Parser>::parse();
        crate::agent::run(&crate::config::resolve(cli.config)?, running)
    })();
    handle.set_service_status(status(
        ServiceState::Stopped,
        if result.is_ok() { 0 } else { 1 },
    ))?;
    result
}
pub fn install(config: &Path) -> Result<()> {
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)
            .context("Installing a Windows service requires an elevated terminal")?;
    manager.create_service(
        &ServiceInfo {
            name: NAME.into(),
            display_name: "ngxd folder watcher".into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: std::env::current_exe()?,
            launch_arguments: vec![
                "--config".into(),
                config.as_os_str().to_owned(),
                "service-run".into(),
            ],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        },
        ServiceAccess::QUERY_STATUS,
    )?;
    println!(
        "Installed ngxd as LocalSystem with config {}. Configure folder and api_key, then run `ngxd start`.",
        config.display()
    );
    Ok(())
}
pub fn control(action: &str) -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let access = match action {
        "start" => ServiceAccess::START,
        "stop" => ServiceAccess::STOP,
        _ => ServiceAccess::QUERY_STATUS,
    };
    let service = manager.open_service(NAME, access)?;
    match action {
        "start" => service.start::<OsString>(&[])?,
        "stop" => {
            service.stop()?;
        }
        "status" => println!("{:?}", service.query_status()?.current_state),
        _ => anyhow::bail!("Unsupported action"),
    }
    Ok(())
}
pub fn uninstall() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        NAME,
        ServiceAccess::DELETE | ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
    )?;
    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
    }
    service.delete()?;
    Ok(())
}
