//! The Conductor Windows service.
//!
//! Runs as LocalSystem from boot. It does no capture itself: it keeps one
//! agent process running in the console session while hosting is on, and
//! answers the local app over a named pipe. With nobody connected the agent
//! sits idle, so the pair costs next to nothing.

use std::ffi::{OsString, c_void};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use conductor_proto::{CONTROL_PIPE, ControlRequest, ControlStatus, DEFAULT_PORT, HostInfo, UpdateStatus};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{Mutex, Notify, mpsc, watch};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::core::w;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::settings::Settings;
use crate::spawn::{Child, Job, console_session, spawn_in_session};
use crate::update::{self, Update};

pub const SERVICE_NAME: &str = "Conductor";

/// An agent that dies sooner than this after starting counts as a crash.
const HEALTHY_RUN: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const RECONCILE_EVERY: Duration = Duration::from_secs(2);
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(60);
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// How often a downloaded update looks for a moment when nobody is using Conductor.
const INSTALL_RETRY: Duration = Duration::from_secs(5 * 60);

define_windows_service!(ffi_service_main, service_main);

/// Hands the process to the service control manager. Only returns once the service stops.
pub fn run() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).context("not started by the service manager")?;
    Ok(())
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!("service failed: {e:#}");
    }
}

fn run_service() -> Result<()> {
    let (stop_tx, stop_rx) = watch::channel(false);
    let wake = Arc::new(Notify::new());
    let wake_handler = wake.clone();
    let handler = move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            _ = stop_tx.send(true);
            ServiceControlHandlerResult::NoError
        }
        // Sign-in, sign-out, user switching and console reconnects.
        ServiceControl::SessionChange(_) => {
            wake_handler.notify_one();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state, controls_accepted| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        })
    };
    report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN | ServiceControlAccept::SESSION_CHANGE,
    )?;
    tracing::info!("service started (version {})", env!("CARGO_PKG_VERSION"));

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let result = rt.block_on(supervise(stop_rx, wake));
    // Don't wait for an installer that is replacing this service.
    rt.shutdown_timeout(Duration::from_secs(2));
    tracing::info!("service stopped");
    report(ServiceState::Stopped, ServiceControlAccept::empty())?;
    result
}

struct Supervisor {
    settings: Settings,
    exe: PathBuf,
    job: Job,
    agent: Option<(Child, Instant)>,
    failures: u32,
    next_attempt: Instant,
    update: UpdateStatus,
    updates: mpsc::Sender<UpdateRequest>,
}

enum UpdateRequest {
    Check,
    Install,
}

impl Supervisor {
    /// Makes reality match the settings: one healthy agent in the console
    /// session while hosting is on, none otherwise.
    fn reconcile(&mut self) {
        let target = if self.settings.hosting { console_session() } else { None };
        if let Some((agent, started)) = &self.agent {
            let alive = agent.is_running();
            if alive && Some(agent.session) == target {
                if started.elapsed() > HEALTHY_RUN {
                    self.failures = 0;
                }
                return;
            }
            if alive {
                tracing::info!("stopping agent {} in session {}", agent.pid, agent.session);
                agent.kill();
            } else {
                tracing::warn!("agent {} exited with code {:?}", agent.pid, agent.exit_code());
                if started.elapsed() < HEALTHY_RUN {
                    self.failures += 1;
                    let backoff = Duration::from_secs(1 << self.failures.min(6)).min(MAX_BACKOFF);
                    self.next_attempt = Instant::now() + backoff;
                }
            }
            self.agent = None;
        }
        let Some(session) = target else { return };
        if Instant::now() < self.next_attempt {
            return;
        }
        match spawn_in_session(&self.job, session, &self.exe, "agent") {
            Ok(child) => {
                tracing::info!("started agent {} in session {session}", child.pid);
                self.agent = Some((child, Instant::now()));
            }
            Err(e) => {
                tracing::error!("starting agent in session {session}: {e:#}");
                self.failures += 1;
                self.next_attempt = Instant::now() + MAX_BACKOFF.min(Duration::from_secs(5 * self.failures as u64));
            }
        }
    }

    fn status(&self) -> ControlStatus {
        ControlStatus {
            hosting: self.settings.hosting,
            agent_running: self.agent_running(),
            version: env!("CARGO_PKG_VERSION").into(),
            auto_update: self.settings.auto_update,
            update: self.update.clone(),
        }
    }

    fn agent_running(&self) -> bool {
        self.agent.as_ref().is_some_and(|(a, _)| a.is_running())
    }

    fn stop_agent(&mut self) {
        if let Some((agent, _)) = self.agent.take() {
            agent.kill();
        }
    }
}

async fn supervise(mut stop: watch::Receiver<bool>, wake: Arc<Notify>) -> Result<()> {
    let exe = std::env::current_exe()?;
    let failed_update = exe.parent().and_then(|dir| update::after_start(dir.join(update::APP_EXE)));
    let (updates, update_requests) = mpsc::channel(4);
    let sup = Arc::new(Mutex::new(Supervisor {
        settings: Settings::load(),
        exe,
        job: Job::new()?,
        agent: None,
        failures: 0,
        next_attempt: Instant::now(),
        update: UpdateStatus::NotChecked,
        updates,
    }));
    let pipe = tokio::spawn(serve_pipe(sup.clone(), wake.clone()));
    let updater = tokio::spawn(keep_updated(sup.clone(), update_requests, failed_update));

    loop {
        sup.lock().await.reconcile();
        tokio::select! {
            _ = stop.changed() => break,
            _ = wake.notified() => {}
            _ = tokio::time::sleep(RECONCILE_EVERY) => {}
        }
    }
    pipe.abort();
    updater.abort();
    sup.lock().await.stop_agent();
    Ok(())
}

/// Checks for updates now and then, and installs them when nobody is using
/// Conductor: no one is controlling this PC and the app is closed here.
async fn keep_updated(sup: Arc<Mutex<Supervisor>>, mut requests: mpsc::Receiver<UpdateRequest>, failed: Option<String>) {
    let mut ready: Option<Update> = None;
    let mut next_check = Instant::now() + FIRST_UPDATE_CHECK;
    if let Some(version) = failed {
        // Not straight away, or an update that can't install would keep restarting this service.
        next_check = Instant::now() + UPDATE_CHECK_EVERY;
        sup.lock().await.update = UpdateStatus::Failed { message: format!("Conductor {version} didn't install.") };
    }
    loop {
        let wait = if ready.is_some() { INSTALL_RETRY } else { next_check.saturating_duration_since(Instant::now()) };
        let request = tokio::select! {
            r = requests.recv() => match r {
                Some(r) => Some(r),
                None => return,
            },
            _ = tokio::time::sleep(wait) => None,
        };
        let auto = sup.lock().await.settings.auto_update;
        let install_now = matches!(request, Some(UpdateRequest::Install));
        let check = match request {
            Some(UpdateRequest::Check) => true,
            Some(UpdateRequest::Install) => ready.is_none(),
            None => auto && ready.is_none() && Instant::now() >= next_check,
        };

        if check {
            if ready.is_none() {
                // The last installer is usually still finishing when the new
                // service starts, so its download is removed here instead.
                update::remove_downloads();
            }
            sup.lock().await.update = UpdateStatus::Checking;
            let result = tokio::task::spawn_blocking(update::fetch).await.unwrap_or_else(|e| Err(e.into()));
            next_check = Instant::now() + UPDATE_CHECK_EVERY;
            let status = match result {
                Ok(Some(u)) => {
                    let status = UpdateStatus::Ready { version: u.version.clone() };
                    ready = Some(u);
                    status
                }
                Ok(None) => UpdateStatus::UpToDate,
                Err(e) => {
                    tracing::warn!("checking for updates: {e:#}");
                    UpdateStatus::Failed { message: format!("{e:#}") }
                }
            };
            sup.lock().await.update = status;
        }

        let Some(u) = &ready else { continue };
        if !install_now && !(auto && idle(&sup).await) {
            continue;
        }
        sup.lock().await.update = UpdateStatus::Installing { version: u.version.clone() };
        let u = ready.take().expect("checked above");
        // Only returns if the installer failed; on success it replaces this service.
        let result = tokio::task::spawn_blocking(move || update::install(&u)).await;
        let message = match result {
            Ok(Err(e)) => format!("{e:#}"),
            Ok(Ok(())) => "the installer didn't finish".into(),
            Err(e) => e.to_string(),
        };
        tracing::warn!("installing the update: {message}");
        sup.lock().await.update = UpdateStatus::Failed { message };
    }
}

/// Nobody is controlling this PC and the Conductor app isn't open.
async fn idle(sup: &Mutex<Supervisor>) -> bool {
    if !update::app_sessions().is_empty() {
        return false;
    }
    if !sup.lock().await.agent_running() {
        return true;
    }
    let agent = std::net::SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT));
    match conductor_tailnet::get_json::<HostInfo>(agent, "/api/info").await {
        Ok(info) => info.controllers.is_empty(),
        Err(_) => false,
    }
}

/// Lets SYSTEM and administrators do anything, and anyone signed in at this
/// PC read the status and flip hosting on or off. Remote clients are refused.
struct PipeSecurity(SECURITY_ATTRIBUTES);

// Points at a descriptor that is never freed or mutated.
unsafe impl Send for PipeSecurity {}

fn pipe_security() -> Result<PipeSecurity> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)"),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .context("building pipe security descriptor")?;
    }
    // The descriptor lives for the rest of the process.
    Ok(PipeSecurity(SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    }))
}

fn create_pipe(first: bool, sa: &mut PipeSecurity) -> std::io::Result<NamedPipeServer> {
    let mut opts = ServerOptions::new();
    // Refuse to start if someone else already owns the name.
    opts.first_pipe_instance(first).reject_remote_clients(true);
    unsafe { opts.create_with_security_attributes_raw(CONTROL_PIPE, &mut sa.0 as *mut _ as *mut c_void) }
}

async fn serve_pipe(sup: Arc<Mutex<Supervisor>>, wake: Arc<Notify>) -> Result<()> {
    let mut sa = pipe_security()?;
    let mut server = create_pipe(true, &mut sa).context("creating control pipe")?;
    loop {
        server.connect().await?;
        let client = std::mem::replace(&mut server, create_pipe(false, &mut sa)?);
        let sup = sup.clone();
        let wake = wake.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(client, sup, wake).await {
                tracing::debug!("control client: {e:#}");
            }
        });
    }
}

async fn handle_client(pipe: NamedPipeServer, sup: Arc<Mutex<Supervisor>>, wake: Arc<Notify>) -> Result<()> {
    let mut reader = BufReader::new(pipe);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await??;
    let request: ControlRequest = serde_json::from_str(line.trim())?;
    let status = {
        let mut sup = sup.lock().await;
        match request {
            ControlRequest::Status => {}
            ControlRequest::SetHosting { enabled } => {
                if sup.settings.hosting != enabled {
                    sup.settings.hosting = enabled;
                    sup.settings.save()?;
                    tracing::info!("hosting turned {}", if enabled { "on" } else { "off" });
                    sup.failures = 0;
                    sup.next_attempt = Instant::now();
                    wake.notify_one();
                }
            }
            ControlRequest::SetAutoUpdate { enabled } => {
                if sup.settings.auto_update != enabled {
                    sup.settings.auto_update = enabled;
                    sup.settings.save()?;
                    tracing::info!("automatic updates turned {}", if enabled { "on" } else { "off" });
                }
            }
            ControlRequest::CheckForUpdates => {
                if !matches!(sup.update, UpdateStatus::Checking | UpdateStatus::Installing { .. }) {
                    sup.update = UpdateStatus::Checking;
                    _ = sup.updates.try_send(UpdateRequest::Check);
                }
            }
            ControlRequest::InstallUpdate => {
                if !matches!(sup.update, UpdateStatus::Installing { .. }) {
                    _ = sup.updates.try_send(UpdateRequest::Install);
                }
            }
        }
        sup.status()
    };
    let mut reply = serde_json::to_vec(&status)?;
    reply.push(b'\n');
    let pipe = reader.get_mut();
    pipe.write_all(&reply).await?;
    pipe.flush().await?;
    Ok(())
}
