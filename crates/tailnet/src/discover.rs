//! Finds the owner's machines on the tailnet and asks each one whether
//! Conductor is running there.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use conductor_proto::{DEFAULT_PORT, Device, HostInfo};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::cli::Status;
use crate::memory::WakeMemory;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1200);
const SELF_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE: usize = 256 * 1024;

/// Operating systems Conductor can run on as a host.
fn can_host(os: &str) -> bool {
    os.eq_ignore_ascii_case("windows")
}

/// Lists the owner's machines that can run Conductor, probing the online ones.
/// `requester` marks the machine that asked, so it can leave itself out.
/// How to wake each one is kept in `memory` for when it's asleep.
pub async fn discover(status: &Status, requester: Option<IpAddr>, memory: &WakeMemory) -> Vec<Device> {
    let Some(owner) = status.owner() else { return Vec::new() };
    let me = status.self_node.as_ref();
    let nodes = me
        .into_iter()
        .map(|n| (n, true))
        .chain(status.peer.values().map(|n| (n, false)))
        .filter(|(n, _)| n.user_id == owner && can_host(&n.os));

    let probes = nodes.map(|(node, is_self)| async move {
        let ip = node.ipv4();
        let host = match ip {
            Some(ip) if node.online || is_self => probe(ip, is_self).await.ok(),
            _ => None,
        };
        let wake = match host.as_ref().and_then(|h| h.wake.as_ref()) {
            Some(wake) => {
                memory.remember(&node.id, wake);
                Some(wake.clone())
            }
            None => memory.recall(&node.id),
        };
        Device {
            id: node.id.clone(),
            name: node.host_name.clone(),
            dns_name: node.fqdn().to_string(),
            address: ip.map(|ip| ip.to_string()).unwrap_or_default(),
            os: node.os.clone(),
            online: node.online || is_self,
            last_seen: if node.online || is_self { None } else { node.last_seen().map(str::to_string) },
            host,
            silent: false,
            wake,
            is_self,
            is_you: requester.is_some_and(|r| node.tailscale_ips.contains(&r)),
        }
    });
    let mut devices = futures::future::join_all(probes).await;
    // Tailscale can list a PC as online for hours after it went to sleep. If
    // one didn't answer the probe, ask whether it answered the tunnel at all.
    if devices.iter().any(|d| d.online && !d.is_self && d.host.is_none()) {
        if let Ok(now) = crate::cli::status().await {
            for d in devices.iter_mut().filter(|d| d.online && !d.is_self && d.host.is_none()) {
                d.silent = now.peer.values().find(|n| n.id == d.id).is_some_and(|n| !n.answered_lately());
            }
        }
    }
    devices.sort_by(|a, b| {
        (b.host.is_some(), b.online).cmp(&(a.host.is_some(), a.online)).then_with(|| a.name.cmp(&b.name))
    });
    devices
}

async fn probe(ip: IpAddr, is_self: bool) -> Result<HostInfo> {
    // This PC is asked directly, and given longer: a slow answer from itself
    // isn't the network, just a busy moment.
    if is_self {
        let local = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), DEFAULT_PORT);
        return get_json_within(local, "/api/info", SELF_PROBE_TIMEOUT).await;
    }
    get_json(SocketAddr::new(ip, DEFAULT_PORT), "/api/info").await
}

/// Minimal HTTP/1.1 GET for talking to other Conductor hosts. They only serve
/// small JSON bodies with a Content-Length, so a full client isn't needed.
pub async fn get_json<T: DeserializeOwned>(addr: SocketAddr, path: &str) -> Result<T> {
    get_json_within(addr, path, PROBE_TIMEOUT).await
}

async fn get_json_within<T: DeserializeOwned>(addr: SocketAddr, path: &str, timeout: Duration) -> Result<T> {
    tokio::time::timeout(timeout, async {
        let mut stream = TcpStream::connect(addr).await?;
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        let mut buf = Vec::with_capacity(4096);
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > MAX_RESPONSE {
                bail!("response too large");
            }
        }
        let split = buf.windows(4).position(|w| w == b"\r\n\r\n").context("malformed response")?;
        let head = std::str::from_utf8(&buf[..split]).context("malformed headers")?;
        let status = head.split_whitespace().nth(1).context("malformed status line")?;
        if status != "200" {
            bail!("HTTP {status}");
        }
        Ok(serde_json::from_slice(&buf[split + 4..])?)
    })
    .await
    .context("timed out")?
}
