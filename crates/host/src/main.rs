//! Conductor host: makes this PC controllable from your other tailnet devices.
//!
//! ```text
//! conductor-host              serve in the foreground (development)
//! conductor-host install      register the service and firewall rule (admin)
//! conductor-host uninstall    remove them (admin)
//! conductor-host service      entry point for the Windows service manager
//! conductor-host agent        started by the service in the console session
//! ```

mod capture;
mod convert;
mod desktop;
mod displays;
mod encoder;
mod input;
mod install;
mod pipeline;
mod server;
mod service;
mod session;
mod settings;
mod spawn;
mod state;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

#[derive(PartialEq)]
enum Mode {
    Foreground,
    Agent,
    Service,
    Install,
    Uninstall,
}

struct Args {
    mode: Mode,
    port: u16,
    web_dir: PathBuf,
}

fn parse_args() -> Result<Args> {
    let mut mode = Mode::Foreground;
    let mut port = conductor_proto::DEFAULT_PORT;
    let mut web_dir = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "agent" => mode = Mode::Agent,
            "service" => mode = Mode::Service,
            "install" => mode = Mode::Install,
            "uninstall" => mode = Mode::Uninstall,
            "--port" => port = it.next().context("--port needs a value")?.parse()?,
            "--web-dir" => web_dir = Some(PathBuf::from(it.next().context("--web-dir needs a value")?)),
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let web_dir = web_dir.unwrap_or_else(|| {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf()));
        match exe_dir.map(|d| d.join("web")) {
            Some(d) if d.join("index.html").exists() => d,
            _ => PathBuf::from("app/dist"),
        }
    });
    Ok(Args { mode, port, web_dir })
}

fn init_logging(file: Option<&str>) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,str0m=warn"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match file.map(settings::log_file) {
        Some(Ok(f)) => builder.with_ansi(false).with_writer(Mutex::new(f)).init(),
        _ => builder.init(),
    }
}

fn main() -> Result<()> {
    let args = parse_args()?;
    match args.mode {
        Mode::Install => {
            init_logging(None);
            install::install()
        }
        Mode::Uninstall => {
            init_logging(None);
            install::uninstall()
        }
        Mode::Service => {
            init_logging(Some("service.log"));
            service::run()
        }
        Mode::Agent | Mode::Foreground => {
            init_logging((args.mode == Mode::Agent).then_some("agent.log"));
            let rt = tokio::runtime::Runtime::new()?;
            let result = rt.block_on(serve(args));
            if let Err(e) = &result {
                tracing::error!("{e:#}");
            }
            result
        }
    }
}

fn dpi_aware() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    unsafe { _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

async fn wait_for_tailscale() -> conductor_tailnet::Status {
    let mut warned = false;
    loop {
        match conductor_tailnet::status().await {
            Ok(s) if s.is_running() => return s,
            Ok(s) if !warned => tracing::warn!("waiting for Tailscale (state: {})", s.backend_state),
            Err(e) if !warned => tracing::warn!("waiting for Tailscale: {e:#}"),
            _ => {}
        }
        warned = true;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn serve(args: Args) -> Result<()> {
    dpi_aware();
    str0m::crypto::from_feature_flags().install_process_default();

    let status = wait_for_tailscale().await;
    let ip = status.self_node.as_ref().and_then(|n| n.ipv4()).context("no Tailscale IPv4 address")?;
    let state = state::AppState::new(status, ip, args.web_dir.clone())?;

    let app = server::router(state.clone());
    let mut servers = Vec::new();
    for addr in [SocketAddr::new(ip, args.port), SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), args.port)] {
        let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
        let app = app.clone().into_make_service_with_connect_info::<SocketAddr>();
        servers.push(tokio::spawn(async move { axum::serve(listener, app).await }));
    }
    tracing::info!(
        "Conductor host ready: http://{}:{} (web client from {})",
        state.dns_label,
        args.port,
        args.web_dir.display()
    );
    for s in servers {
        s.await??;
    }
    Ok(())
}
