//! Wake-on-LAN: what other PCs need to wake this one, and the packets that
//! wake them.

use std::fs;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use conductor_proto::{WakeAdapter, WakeInfo};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, GetIfEntry2,
    IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
};
use windows::Win32::NetworkManagement::Ndis::{IfOperStatusUp, NET_LUID_LH};
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows::Win32::System::Power::{CallNtPowerInformation, LastWakeTime};
use windows::Win32::System::WindowsProgramming::QueryInterruptTime;

use crate::settings::data_dir;

/// A test wake counts if this PC woke this recently.
const RECENT_WAKE: Duration = Duration::from_secs(10 * 60);

/// What another PC needs to wake this one.
pub fn info() -> WakeInfo {
    let adapters = adapters().unwrap_or_else(|e| {
        tracing::warn!("{e:#}");
        Vec::new()
    });
    let verified = Saved::load().verified_any(&adapters);
    WakeInfo { adapters, verified }
}

/// Wired and Wi-Fi adapters that are connected, with their IPv4 address on
/// the local network. Virtual adapters (VPNs, Hyper-V) can't wake anything.
pub fn adapters() -> Result<Vec<WakeAdapter>> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16 * 1024u32;
    let buf = loop {
        // u64s keep the adapter structs aligned.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let r = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(buf.as_mut_ptr().cast()), &mut size) };
        match r {
            r if r == NO_ERROR.0 => break buf,
            r if r == ERROR_BUFFER_OVERFLOW.0 => continue,
            r => bail!("listing network adapters: error {r}"),
        }
    };
    let mut found = Vec::new();
    let mut next = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while let Some(a) = unsafe { next.as_ref() } {
        next = a.Next;
        let wired_or_wifi = matches!(a.IfType, IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211);
        if !wired_or_wifi || a.PhysicalAddressLength != 6 || a.OperStatus != IfOperStatusUp || !is_hardware(a.Luid) {
            continue;
        }
        let mac = a.PhysicalAddress[..6].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
        let mut unicast = a.FirstUnicastAddress;
        while let Some(u) = unsafe { unicast.as_ref() } {
            unicast = u.Next;
            let sockaddr = u.Address.lpSockaddr;
            if sockaddr.is_null() || unsafe { (*sockaddr).sa_family } != AF_INET {
                continue;
            }
            let b = unsafe { (*(sockaddr as *const SOCKADDR_IN)).sin_addr.S_un.S_un_b };
            let address = Ipv4Addr::new(b.s_b1, b.s_b2, b.s_b3, b.s_b4);
            if address.is_link_local() || address.is_loopback() {
                continue;
            }
            found.push(WakeAdapter { mac: mac.clone(), address, prefix: u.OnLinkPrefixLength });
            break;
        }
    }
    Ok(found)
}

fn is_hardware(luid: NET_LUID_LH) -> bool {
    let mut row = MIB_IF_ROW2 { InterfaceLuid: luid, ..Default::default() };
    // The first flag bit is HardwareInterface.
    let found = unsafe { GetIfEntry2(&mut row) } == NO_ERROR;
    found && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0
}

/// Sends magic packets to each of `targets` that's on one of this PC's
/// networks: broadcast on that network, and straight to its last address,
/// which reaches adapters that answer for themselves while asleep.
pub async fn send(targets: &[WakeAdapter]) -> Result<()> {
    let ours = adapters()?;
    let mut sends = Vec::new();
    for target in targets {
        let Some(local) = ours.iter().find(|o| o.shares_network(target)) else { continue };
        let socket = UdpSocket::bind((local.address, 0)).await.context("opening a socket to send wake packets")?;
        socket.set_broadcast(true)?;
        let (network, prefix) = target.network();
        let broadcast = Ipv4Addr::from(network | (u32::MAX.checked_shr(u32::from(prefix)).unwrap_or(0)));
        let destinations = [Ipv4Addr::BROADCAST, broadcast, target.address];
        sends.push((socket, magic_packet(&target.mac)?, destinations));
        tracing::info!("waking {} from {}", target.mac, local.address);
    }
    if sends.is_empty() {
        bail!("This PC isn't on the same network as the PC to wake.");
    }
    // Repeated in case one is lost: they're small and only sent on request.
    for round in 0..3 {
        if round > 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        for (socket, packet, destinations) in &sends {
            for address in destinations {
                for port in [9, 7] {
                    if let Err(e) = socket.send_to(packet, (*address, port)).await {
                        tracing::debug!("wake packet to {address}:{port}: {e}");
                    }
                }
            }
        }
    }
    Ok(())
}

fn magic_packet(mac: &str) -> Result<Vec<u8>> {
    let bytes = mac
        .split([':', '-'])
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<Vec<u8>, _>>()
        .ok()
        .filter(|b| b.len() == 6)
        .with_context(|| format!("not a MAC address: {mac}"))?;
    let mut packet = vec![0xff; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&bytes);
    }
    Ok(packet)
}

/// Records that a test wake worked, once sure this PC really did just wake up.
pub fn confirm() -> Result<WakeInfo> {
    if !woke_recently() {
        bail!("This PC hasn't woken from sleep in the last few minutes.");
    }
    let adapters = adapters()?;
    let mut saved = Saved::load();
    for a in &adapters {
        if !saved.verified.contains(&a.mac) {
            saved.verified.push(a.mac.clone());
        }
    }
    saved.save().context("saving that waking works")?;
    tracing::info!("waking this PC over the network works");
    Ok(WakeInfo { adapters, verified: true })
}

/// When this process last saw the PC resume, from [`watch_resume`].
static RESUMED: Mutex<Option<Instant>> = Mutex::new(None);

fn woke_recently() -> bool {
    if RESUMED.lock().unwrap().is_some_and(|at| at.elapsed() < RECENT_WAKE) {
        return true;
    }
    // In case this process started after the PC woke up. Both counts are in
    // 100 ns units and include time spent asleep.
    let mut woke: u64 = 0;
    let status = unsafe {
        CallNtPowerInformation(LastWakeTime, None, 0, Some(&mut woke as *mut u64 as *mut _), size_of::<u64>() as u32)
    };
    if status.is_err() || woke == 0 {
        return false;
    }
    let now = unsafe { QueryInterruptTime() };
    now.checked_sub(woke).is_some_and(|since| since < RECENT_WAKE.as_nanos() as u64 / 100)
}

/// Notes each time the PC resumes from sleep, for as long as this process runs.
pub fn watch_resume() {
    use windows::Win32::Foundation::{HANDLE, WIN32_ERROR};
    use windows::Win32::System::Power::{DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, PowerRegisterSuspendResumeNotification};
    use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND};

    unsafe extern "system" fn on_power(_: *const core::ffi::c_void, kind: u32, _: *const core::ffi::c_void) -> u32 {
        if kind == PBT_APMRESUMEAUTOMATIC || kind == PBT_APMRESUMESUSPEND {
            *RESUMED.lock().unwrap() = Some(Instant::now());
        }
        0
    }

    // Registered once and never removed, so the parameters live forever too.
    let params = Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(on_power),
        Context: std::ptr::null_mut(),
    }));
    let mut handle = std::ptr::null_mut();
    let r: WIN32_ERROR = unsafe {
        PowerRegisterSuspendResumeNotification(DEVICE_NOTIFY_CALLBACK, HANDLE(params as *mut _ as *mut _), &mut handle)
    };
    if r != NO_ERROR {
        tracing::warn!("watching for the PC waking up: error {}", r.0);
    }
}

/// Adapters through which a test wake has worked, by MAC address.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Saved {
    verified: Vec<String>,
}

impl Saved {
    fn path() -> PathBuf {
        data_dir().join("wake.json")
    }

    fn load() -> Self {
        fs::read(Self::path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    fn save(&self) -> Result<()> {
        fs::create_dir_all(data_dir())?;
        let tmp = Self::path().with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, Self::path())?;
        Ok(())
    }

    fn verified_any(&self, adapters: &[WakeAdapter]) -> bool {
        adapters.iter().any(|a| self.verified.contains(&a.mac))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets() {
        let packet = magic_packet("a4:b1:C2:d3:e4:f5").unwrap();
        assert_eq!(packet.len(), 102);
        assert_eq!(&packet[..6], &[0xff; 6]);
        assert_eq!(&packet[96..], &[0xa4, 0xb1, 0xc2, 0xd3, 0xe4, 0xf5]);
        assert_eq!(magic_packet("a4-b1-c2-d3-e4-f5").unwrap(), packet);
        assert!(magic_packet("a4:b1:c2").is_err());
        assert!(magic_packet("zz:b1:c2:d3:e4:f5").is_err());
    }

    #[test]
    fn lists_adapters() {
        // Whatever this machine has, every entry is a usable local address.
        for a in adapters().unwrap() {
            assert_eq!(a.mac.len(), 17, "{a:?}");
            assert!(!a.address.is_loopback() && !a.address.is_link_local(), "{a:?}");
            assert!(a.prefix <= 32, "{a:?}");
        }
    }
}
