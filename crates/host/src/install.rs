//! Registers and removes the service and its firewall rule. Needs admin;
//! the installer runs these.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl, ServiceFailureActions,
    ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use crate::service::SERVICE_NAME;

const FIREWALL_RULE: &str = "Conductor";
/// Tailscale's address ranges. Nothing outside the tailnet can reach the host.
const TAILNET_RANGES: &str = "100.64.0.0/10,fd7a:115c:a1e0::/48";
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_MARKED_FOR_DELETE: i32 = 1072;

pub fn install() -> Result<()> {
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
            .context("opening the service manager (run as administrator)")?;
    remove_service(&manager)?;

    let exe = std::env::current_exe()?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: "Conductor".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.clone(),
        launch_arguments: vec!["service".into()],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    // Deleting a service finishes asynchronously; retry briefly if it is still going.
    let deadline = Instant::now() + Duration::from_secs(10);
    let service = loop {
        match manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START) {
            Ok(s) => break s,
            Err(windows_service::Error::Winapi(e))
                if e.raw_os_error() == Some(ERROR_SERVICE_MARKED_FOR_DELETE) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(e).context("creating the service"),
        }
    };
    service.set_description("Lets your other devices on your tailnet see and control this PC.")?;
    let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) };
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    })?;

    allow_through_firewall(&exe)?;
    service.start::<&str>(&[]).context("starting the service")?;
    println!("Conductor service installed and started.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("opening the service manager (run as administrator)")?;
    remove_service(&manager)?;
    _ = netsh(&["delete", "rule", &format!("name={FIREWALL_RULE}")]);
    println!("Conductor service removed.");
    Ok(())
}

/// Stops and deletes the service if it exists.
fn remove_service(manager: &ServiceManager) -> Result<()> {
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(s) => s,
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            return Ok(());
        }
        Err(e) => return Err(e).context("opening the existing service"),
    };
    if service.query_status()?.current_state != ServiceState::Stopped {
        _ = service.stop();
        let deadline = Instant::now() + Duration::from_secs(15);
        while service.query_status()?.current_state != ServiceState::Stopped {
            if Instant::now() > deadline {
                bail!("the service didn't stop in time");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    service.delete().context("deleting the service")?;
    Ok(())
}

fn allow_through_firewall(exe: &Path) -> Result<()> {
    _ = netsh(&["delete", "rule", &format!("name={FIREWALL_RULE}")]);
    netsh(&[
        "add",
        "rule",
        &format!("name={FIREWALL_RULE}"),
        "dir=in",
        "action=allow",
        &format!("program={}", exe.display()),
        "enable=yes",
        "profile=any",
        &format!("remoteip={TAILNET_RANGES}"),
    ])
    .context("adding the firewall rule")
}

fn netsh(args: &[&str]) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new("netsh")
        .args(["advfirewall", "firewall"])
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("running netsh")?;
    if !out.status.success() {
        bail!("netsh: {}", String::from_utf8_lossy(&out.stdout).trim());
    }
    Ok(())
}
