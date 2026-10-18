//! Finding PCs that run Conductor on the same local network, for a phone that
//! doesn't know any yet. Hosts announce themselves with DNS-SD over multicast
//! DNS; [`find`] asks and collects the answers. Each answer carries the PC's
//! tailnet address, where its API is.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The DNS-SD service hosts announce.
pub const SERVICE: &str = "_conductor._tcp.local";
/// TXT key for the host's tailnet address.
pub const TXT_ADDRESS: &str = "address";

const MDNS: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353));
const PTR: u16 = 12;
const TXT: u16 = 16;
/// Class IN, with the top bit asking for the answer straight back (RFC 6762,
/// section 5.4). Windows ignores questions without it from other ports.
const IN_UNICAST: u16 = 0x8001;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NearbyPc {
    /// What the PC calls itself.
    pub name: String,
    /// Its tailnet address.
    pub address: String,
}

/// Asks the local network for PCs running Conductor, and collects answers for `wait`.
pub fn find(wait: Duration) -> std::io::Result<Vec<NearbyPc>> {
    // Answers come straight back to this port rather than to the multicast
    // group, which phones filter unless an app holds a lock on Wi-Fi.
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_multicast_ttl_v4(255)?;
    let start = Instant::now();
    let mut asked_again = false;
    socket.send_to(&query(SERVICE, PTR), MDNS)?;
    let mut instances = HashSet::new();
    let mut found = HashMap::new();
    let mut buf = [0u8; 9000];
    loop {
        let elapsed = start.elapsed();
        if elapsed >= wait {
            break;
        }
        // Once more halfway, in case the first question or an answer got lost.
        if !asked_again && elapsed >= wait / 2 {
            asked_again = true;
            socket.send_to(&query(SERVICE, PTR), MDNS)?;
        }
        let left = if asked_again { wait - elapsed } else { wait / 2 - elapsed };
        socket.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
        let n = match socket.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        let Some(records) = parse(&buf[..n]) else { continue };
        let mut new = Vec::new();
        for r in &records {
            match &r.data {
                Data::Ptr(instance) if r.name.eq_ignore_ascii_case(SERVICE) => {
                    if instances.insert(instance.clone()) {
                        new.push(instance.clone());
                    }
                }
                Data::Txt(entries) => {
                    if let Some(pc) = from_txt(&r.name, entries) {
                        found.insert(r.name.to_ascii_lowercase(), pc);
                    }
                }
                _ => {}
            }
        }
        // Answers usually bring their TXT records along; ask for any that didn't.
        for instance in new {
            if !found.contains_key(&instance.to_ascii_lowercase()) {
                socket.send_to(&query(&instance, TXT), MDNS)?;
            }
        }
    }
    let mut pcs: Vec<_> = found.into_values().collect();
    pcs.sort_by_key(|pc| pc.name.to_lowercase());
    Ok(pcs)
}

fn from_txt(instance: &str, entries: &[String]) -> Option<NearbyPc> {
    let suffix = format!(".{SERVICE}");
    let name = instance.len().checked_sub(suffix.len()).and_then(|at| {
        instance[at..].eq_ignore_ascii_case(&suffix).then(|| instance[..at].to_string())
    })?;
    let address = entries.iter().find_map(|e| {
        let (key, value) = e.split_once('=')?;
        key.eq_ignore_ascii_case(TXT_ADDRESS).then(|| value.to_string())
    })?;
    // Only a tailnet address, which only the owner's own devices can answer
    // at: anything on the Wi-Fi can answer here, and a fake PC on it could
    // show a fake sign-in screen to type a password into.
    let ip = address.parse::<Ipv4Addr>().ok()?;
    let [a, b, ..] = ip.octets();
    (a == 100 && b & 0xC0 == 64).then_some(NearbyPc { name, address })
}

fn query(name: &str, kind: u16) -> Vec<u8> {
    let mut msg = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.push(0);
    msg.extend_from_slice(&kind.to_be_bytes());
    msg.extend_from_slice(&IN_UNICAST.to_be_bytes());
    msg
}

#[derive(Debug, PartialEq)]
enum Data {
    Ptr(String),
    Txt(Vec<String>),
    Other,
}

#[derive(Debug)]
struct Record {
    name: String,
    data: Data,
}

/// The records in a DNS response, or None when it isn't one.
fn parse(msg: &[u8]) -> Option<Vec<Record>> {
    let u16_at = |at: usize| Some(u16::from_be_bytes([*msg.get(at)?, *msg.get(at + 1)?]));
    if u16_at(2)? & 0x8000 == 0 {
        return None;
    }
    let questions = u16_at(4)?;
    let records = u16_at(6)? as usize + u16_at(8)? as usize + u16_at(10)? as usize;
    let mut at = 12;
    for _ in 0..questions {
        at = read_name(msg, at)?.1 + 4;
    }
    let mut out = Vec::new();
    for _ in 0..records {
        let (name, next) = read_name(msg, at)?;
        let kind = u16_at(next)?;
        let len = u16_at(next + 8)? as usize;
        let start = next + 10;
        let data = msg.get(start..start + len)?;
        let data = match kind {
            PTR => Data::Ptr(read_name(msg, start)?.0),
            TXT => {
                let mut entries = Vec::new();
                let mut i = 0;
                while i < data.len() {
                    let n = data[i] as usize;
                    entries.push(String::from_utf8_lossy(data.get(i + 1..i + 1 + n)?).into_owned());
                    i += 1 + n;
                }
                Data::Txt(entries)
            }
            _ => Data::Other,
        };
        out.push(Record { name, data });
        at = start + len;
    }
    Some(out)
}

/// A name at `at`, following compression pointers, and where what follows it starts.
fn read_name(msg: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    // Pointers only lead backwards, but a bad message could loop.
    for _ in 0..128 {
        let len = *msg.get(at)? as usize;
        match len {
            0 => {
                return Some((labels.join("."), end.unwrap_or(at + 1)));
            }
            l if l & 0xC0 == 0xC0 => {
                let target = ((l & 0x3F) << 8) | *msg.get(at + 1)? as usize;
                end.get_or_insert(at + 2);
                at = target;
            }
            l => {
                labels.push(String::from_utf8_lossy(msg.get(at + 1..at + 1 + l)?).into_owned());
                at += 1 + l;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(out: &mut Vec<u8>, name: &str) {
        for label in name.split('.') {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
    }

    fn record(out: &mut Vec<u8>, owner: &[u8], kind: u16, data: &[u8]) {
        out.extend_from_slice(owner);
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&[0x80, 1, 0, 0, 0x11, 0x94]);
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
    }

    /// An answer as a responder sends it: the question repeated, a PTR to the
    /// instance, and its TXT record in the additional section, with compressed names.
    fn answer() -> Vec<u8> {
        let mut msg = vec![0, 0, 0x84, 0, 0, 1, 0, 1, 0, 0, 0, 1];
        name(&mut msg, SERVICE);
        msg.extend_from_slice(&[0, 12, 0, 1]);
        // The service name is at 12, in the question.
        let mut ptr = vec![6];
        ptr.extend_from_slice(b"Studio");
        ptr.extend_from_slice(&[0xC0, 12]);
        let instance_at = msg.len() + 2 + 10;
        record(&mut msg, &[0xC0, 12], PTR, &ptr);
        let mut txt = Vec::new();
        for entry in ["address=100.64.0.7", "v=1"] {
            txt.push(entry.len() as u8);
            txt.extend_from_slice(entry.as_bytes());
        }
        record(&mut msg, &[0xC0, instance_at as u8], TXT, &txt);
        msg
    }

    #[test]
    fn reads_an_answer() {
        let records = parse(&answer()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, SERVICE);
        assert_eq!(records[0].data, Data::Ptr(format!("Studio.{SERVICE}")));
        assert_eq!(records[1].name, format!("Studio.{SERVICE}"));
        let Data::Txt(entries) = &records[1].data else { panic!() };
        assert_eq!(
            from_txt(&records[1].name, entries),
            Some(NearbyPc { name: "Studio".into(), address: "100.64.0.7".into() })
        );
    }

    #[test]
    fn ignores_questions_and_junk() {
        assert!(parse(&query(SERVICE, PTR)).is_none());
        assert!(parse(&[0, 0, 0x84]).is_none());
        let mut looped = vec![0, 0, 0x84, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        looped.extend_from_slice(&[0xC0, 12]);
        assert!(parse(&looped).is_none());
    }

    #[test]
    fn wants_an_address() {
        let instance = format!("Studio.{SERVICE}");
        assert_eq!(from_txt(&instance, &["address=studio/evil".into()]), None);
        assert_eq!(from_txt(&instance, &["address=fd7a:115c:a1e0::7".into()]), None);
        assert_eq!(from_txt(&instance, &["address=192.0.2.20".into()]), None);
        assert_eq!(from_txt(&instance, &["address=100.128.0.1".into()]), None);
        assert!(from_txt(&instance, &["address=100.127.255.254".into()]).is_some());
        assert_eq!(from_txt(&instance, &["v=1".into()]), None);
        assert_eq!(from_txt("Studio._other._tcp.local", &["address=100.64.0.7".into()]), None);
    }
}
