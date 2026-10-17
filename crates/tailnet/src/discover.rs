//! Finds the owner's machines on the tailnet and asks each one whether
//! Conductor is running there.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use conductor_proto::{DEFAULT_PORT, Device, HostInfo};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::cli::Status;

const PROBE_TIMEOUT: Duration = Duration::from_millis(1200);
const MAX_RESPONSE: usize = 256 * 1024;

/// Operating systems Conductor can run on as a host.
fn can_host(os: &str) -> bool {
    os.eq_ignore_ascii_case("windows")
}

/// Lists the owner's machines that can run Conductor, probing the online ones.
/// `requester` marks the machine that asked, so it can leave itself out.
pub async fn discover(status: &Status, requester: Option<IpAddr>) -> Vec<Device> {
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
            Some(ip) if node.online || is_self => probe(ip).await.ok(),
            _ => None,
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
            is_self,
            is_you: requester.is_some_and(|r| node.tailscale_ips.contains(&r)),
        }
    });
    let mut devices = futures::future::join_all(probes).await;
    devices.sort_by(|a, b| {
        (b.host.is_some(), b.online).cmp(&(a.host.is_some(), a.online)).then_with(|| a.name.cmp(&b.name))
    });
    devices
}

async fn probe(ip: IpAddr) -> Result<HostInfo> {
    get_json(SocketAddr::new(ip, DEFAULT_PORT), "/api/info").await
}

/// Minimal HTTP/1.1 GET for talking to other Conductor hosts. They only serve
/// small JSON bodies with a Content-Length, so a full client isn't needed.
pub async fn get_json<T: DeserializeOwned>(addr: SocketAddr, path: &str) -> Result<T> {
    tokio::time::timeout(PROBE_TIMEOUT, async {
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
