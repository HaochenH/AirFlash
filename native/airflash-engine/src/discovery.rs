//! Minimal mDNS (RFC 6762) querier for AirPlay receiver discovery.
//!
//! This is the discovery half only: PTR/SRV/A/TXT parsing plus a small UDP
//! transport. It reuses no third-party discovery protocol stack, and it is
//! deliberately transport-injectable so the parser is unit-testable without
//! multicast. Only the fields AirFlash needs are interpreted; everything else
//! is skipped.
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

pub const AIRPLAY_SERVICE: &str = "_airplay._tcp.local";
pub const RAOP_SERVICE: &str = "_raop._tcp.local";
pub const DEFAULT_MULTICAST: ([u8; 4], u16) = ([224, 0, 0, 251], 5353);

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;
const HEADER: usize = 12;

/// One discovered receiver, before any AirFlash-specific filtering.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Service {
    pub instance: String,
    pub service: String,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<Ipv4Addr>,
    pub txt: BTreeMap<String, String>,
}

impl Service {
    pub fn device_id(&self) -> Option<&str> {
        self.txt.get("deviceid").map(String::as_str)
    }
    pub fn model(&self) -> Option<&str> {
        self.txt
            .get("model")
            .or_else(|| self.txt.get("am"))
            .map(String::as_str)
    }
}

/// Encode an unfragmented mDNS query for the service type or instance name.
pub fn query(name: &str, id: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + name.len() + 6);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0x00, 0x00]); // standard query, not truncated
    out.extend_from_slice(&1u16.to_be_bytes()); // one question
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    write_name(&mut out, name);
    out.extend_from_slice(&TYPE_PTR.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    out
}

/// Write a DNS name as uncompressed labels.
fn write_name(out: &mut Vec<u8>, name: &str) {
    for label in name.split('.').filter(|l| !l.is_empty()) {
        let bytes = label.as_bytes();
        out.push(bytes.len().min(63) as u8);
        out.extend_from_slice(&bytes[..bytes.len().min(63)]);
    }
    out.push(0);
}

/// Read a DNS name, following compression pointers. Returns the name and the
/// offset just past the name in the original slice.
fn read_name(packet: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    let mut jumps = 0;
    loop {
        let length = *packet.get(at)?;
        if length & 0xc0 == 0xc0 {
            let pointer = u16::from_be_bytes([*packet.get(at)?, *packet.get(at + 1)?]) & 0x3fff;
            end.get_or_insert(at + 2);
            jumps += 1;
            if jumps > 32 {
                return None;
            }
            at = pointer as usize;
            continue;
        }
        if length & 0xc0 != 0 {
            return None;
        }
        at += 1;
        if length == 0 {
            break;
        }
        let label = packet.get(at..at + length as usize)?;
        labels.push(String::from_utf8_lossy(label).to_string());
        at += length as usize;
    }
    Some((labels.join("."), end.unwrap_or(at)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordData {
    Ptr(String),
    Srv { port: u16, target: String },
    A(Ipv4Addr),
    Txt(Vec<String>),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    name: String,
    data: RecordData,
    ttl: u32,
}

/// Parse every answer/authority/additional record of one DNS message.
/// Only responses (QR set) carry usable answers, so queries are ignored.
fn parse_records(packet: &[u8]) -> Vec<Record> {
    let mut records = Vec::new();
    if packet.len() < HEADER || packet[2] & 0x80 == 0 {
        return records;
    }
    let questions = u16::from_be_bytes([packet[4], packet[5]]) as usize;
    let total = u16::from_be_bytes([packet[6], packet[7]]) as usize
        + u16::from_be_bytes([packet[8], packet[9]]) as usize
        + u16::from_be_bytes([packet[10], packet[11]]) as usize;
    let mut at = HEADER;
    for _ in 0..questions {
        let Some((_, next)) = read_name(packet, at) else {
            return records;
        };
        at = next + 4;
    }
    for _ in 0..total.min(256) {
        let Some((name, next)) = read_name(packet, at) else {
            break;
        };
        at = next;
        let Some(header) = packet.get(at..at + 10) else {
            break;
        };
        let rtype = u16::from_be_bytes([header[0], header[1]]);
        let ttl = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        let length = u16::from_be_bytes([header[8], header[9]]) as usize;
        at += 10;
        let Some(data) = packet.get(at..at + length) else {
            break;
        };
        at += length;
        let parsed = match rtype {
            TYPE_PTR => read_name(packet, at - length)
                .map(|(target, _)| RecordData::Ptr(target)),
            TYPE_SRV if length >= 6 => read_name(packet, at - length + 6).map(|(target, _)| {
                // Priority and weight are intentionally ignored: AirFlash picks
                // the first reachable address instead of running full SRV rules.
                RecordData::Srv {
                    port: u16::from_be_bytes([data[4], data[5]]),
                    target,
                }
            }),
            TYPE_A if length == 4 => data
                .first_chunk::<4>()
                .map(|bytes| RecordData::A(Ipv4Addr::from(*bytes))),
            TYPE_TXT => Some(RecordData::Txt(txt_entries(data))),
            _ => Some(RecordData::Other),
        };
        if let Some(data) = parsed {
            records.push(Record { name, data, ttl });
        }
    }
    records
}

/// TXT RDATA is a sequence of length-prefixed character-strings.
fn txt_entries(data: &[u8]) -> Vec<String> {
    let mut entries = Vec::new();
    let mut at = 0;
    while let Some(&length) = data.get(at) {
        at += 1;
        let end = at + length as usize;
        let Some(entry) = data.get(at..end) else {
            break;
        };
        if !entry.is_empty() {
            entries.push(String::from_utf8_lossy(entry).to_string());
        }
        at = end;
    }
    entries
}

/// Merge records into receiver stubs. A receiver needs an instance name, a
/// service type, an SRV port and at least one A record.
fn assemble(records: &[Record]) -> Vec<Service> {
    let mut services: BTreeMap<(String, String), Service> = BTreeMap::new();
    for record in records {
        match &record.data {
            RecordData::Ptr(target) => {
                let key = (target.clone(), record.name.clone());
                services.entry(key).or_insert_with(|| Service {
                    instance: target.clone(),
                    service: record.name.clone(),
                    ..Service::default()
                });
            }
            RecordData::Srv { port, target } => {
                // The owner name of an SRV record is the service instance.
                for service in services.values_mut() {
                    if service.instance == record.name {
                        service.host = target.clone();
                        service.port = *port;
                    }
                }
            }
            RecordData::A(address) => {
                let host = record.name.clone();
                for service in services.values_mut() {
                    if service.host == host && !service.addresses.contains(address) {
                        service.addresses.push(*address);
                    }
                }
            }
            RecordData::Txt(entries) => {
                for service in services.values_mut() {
                    if service.instance == record.name || service.service == record.name {
                        for entry in entries {
                            if let Some((key, value)) = entry.split_once('=') {
                                service.txt.insert(key.to_string(), value.to_string());
                            } else {
                                service.txt.insert(entry.clone(), String::new());
                            }
                        }
                    }
                }
            }
            RecordData::Other => {}
        }
    }
    let mut out: Vec<Service> = services
        .into_values()
        .filter(|s| s.port > 0 && !s.addresses.is_empty())
        .collect();
    out.sort_by(|a, b| a.instance.cmp(&b.instance));
    out
}

/// Browse the given service types on the mDNS multicast group until the
/// timeout elapses, resolving instances, hosts and addresses.
pub fn discover(
    services: &[&str],
    timeout: Duration,
    target: SocketAddr,
) -> std::io::Result<Vec<Service>> {
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let start = Instant::now();
    let mut records = Vec::new();
    let mut instances: Vec<String> = Vec::new();
    let mut hosts: Vec<String> = Vec::new();
    let mut id = 0u16;
    let mut next_send = Instant::now();
    while start.elapsed() < timeout {
        if Instant::now() >= next_send {
            // Round one asks for the service types; later rounds resolve the
            // instances and hosts that showed up, which is how mDNS responders
            // answer when they do not attach additional records.
            let mut pending: Vec<String> = services.iter().map(|s| (*s).to_string()).collect();
            pending.extend(instances.iter().cloned());
            pending.extend(hosts.iter().cloned());
            for name in pending {
                let _ = socket.send_to(&query(&name, id), target);
                id = id.wrapping_add(1);
            }
            next_send = Instant::now() + Duration::from_millis(750);
        }
        let mut buffer = [0u8; 4096];
        if let Ok((size, _)) = socket.recv_from(&mut buffer) {
            for record in parse_records(&buffer[..size]) {
                match &record.data {
                    RecordData::Ptr(target) => {
                        if !instances.contains(target) {
                            instances.push(target.clone());
                        }
                    }
                    RecordData::Srv { target, .. } if !hosts.contains(target) => {
                        hosts.push(target.clone());
                    }
                    _ => {}
                }
                records.push(record);
            }
        }
    }
    Ok(assemble(&records))
}

#[cfg(test)]
mod tests_support {
    #![allow(dead_code)]
    use super::*;
    pub fn name(parts: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in parts {
            out.push(part.len() as u8);
            out.extend_from_slice(part.as_bytes());
        }
        out.push(0);
        out
    }
    pub fn header(questions: u16, answers: u16) -> Vec<u8> {
        let mut out = vec![0x00, 0x00, 0x84, 0x00];
        out.extend_from_slice(&questions.to_be_bytes());
        out.extend_from_slice(&answers.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }
    pub fn record(out: &mut Vec<u8>, name: &[&str], rtype: u16, data: &[u8]) {
        for part in name {
            out.push(part.len() as u8);
            out.extend_from_slice(part.as_bytes());
        }
        out.push(0);
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&120u32.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(parts: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in parts {
            out.push(part.len() as u8);
            out.extend_from_slice(part.as_bytes());
        }
        out.push(0);
        out
    }

    #[test]
    fn query_encodes_single_question_without_compression() {
        let bytes = query(AIRPLAY_SERVICE, 0x1234);
        assert_eq!(&bytes[..2], &[0x12, 0x34]);
        assert_eq!(&bytes[2..4], &[0x00, 0x00], "must be a standard query");
        assert_eq!(u16::from_be_bytes([bytes[4], bytes[5]]), 1);
        assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 0);
        let mut expected = name(&["_airplay", "_tcp", "local"]);
        expected.extend_from_slice(&TYPE_PTR.to_be_bytes());
        expected.extend_from_slice(&CLASS_IN.to_be_bytes());
        assert_eq!(&bytes[HEADER..], &expected[..]);
    }

    #[test]
    fn read_name_follows_pointers_and_rejects_garbage() {
        let mut packet = name(&["_airplay", "_tcp", "local"]);
        packet.extend_from_slice(&[0xc0, 0x00]); // pointer to offset 0
        let (parsed, end) = read_name(&packet, packet.len() - 2).unwrap();
        assert_eq!(parsed, "_airplay._tcp.local");
        assert_eq!(end, packet.len());
        assert!(read_name(&[0x40, 0x00], 0).is_none());
        let mut looping = vec![0xc0, 0x00];
        looping.extend_from_slice(&[0x00, 0xc0, 0x00]);
        assert!(read_name(&looping, 0).is_none());
    }

    fn header(questions: u16, answers: u16) -> Vec<u8> {
        let mut out = vec![0x00, 0x00, 0x84, 0x00];
        out.extend_from_slice(&questions.to_be_bytes());
        out.extend_from_slice(&answers.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    fn record(out: &mut Vec<u8>, name: &[&str], rtype: u16, data: &[u8]) {
        for part in name {
            out.push(part.len() as u8);
            out.extend_from_slice(part.as_bytes());
        }
        out.push(0);
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&120u32.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
    }

    #[test]
    fn response_with_compressed_names_assembles_a_receiver() {
        let mut packet = header(1, 5);
        // The echoed question name is what response pointers compress against.
        packet.extend_from_slice(&name(&["_airplay", "_tcp", "local"]));
        packet.extend_from_slice(&[0x00, 0x0c, 0x00, 0x01]);
        // PTR answer uses the question's service name through a compression pointer.
        packet.extend_from_slice(&[0xc0, 0x0c]);
        packet.extend_from_slice(&TYPE_PTR.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&120u32.to_be_bytes());
        let ptr = name(&["Living Room", "_airplay", "_tcp", "local"]);
        packet.extend_from_slice(&(ptr.len() as u16).to_be_bytes());
        packet.extend_from_slice(&ptr);
        // SRV + A for the host, then a TXT for the instance.
        record(
            &mut packet,
            &["Living Room", "_airplay", "_tcp", "local"],
            TYPE_SRV,
            &{
                let mut data = vec![0, 0, 0, 0, 0x1f, 0x90];
                data.extend(name(&["homepod-1", "local"]));
                data
            },
        );
        record(&mut packet, &["homepod-1", "local"], TYPE_A, &[192, 168, 1, 42]);
        record(
            &mut packet,
            &["homepod-1", "local"],
            TYPE_TXT,
            &[7, b'd', b'e', b'v', b'i', b'c', b'e'],
        );
        let records = parse_records(&packet);
        let services = assemble(&records);
        assert_eq!(services.len(), 1);
        let service = &services[0];
        assert_eq!(service.instance, "Living Room._airplay._tcp.local");
        assert_eq!(service.service, "_airplay._tcp.local");
        assert_eq!(service.host, "homepod-1.local");
        assert_eq!(service.port, 8080);
        assert_eq!(service.addresses, vec![Ipv4Addr::new(192, 168, 1, 42)]);
    }

    #[test]
    fn incomplete_records_are_not_reported_as_receivers() {
        let mut packet = header(0, 1);
        record(
            &mut packet,
            &["half", "_airplay", "_tcp", "local"],
            TYPE_PTR,
            &name(&["half", "_airplay", "_tcp", "local"]),
        );
        assert!(assemble(&parse_records(&packet)).is_empty());
        let mut packet = header(0, 1);
        record(&mut packet, &["orphan", "local"], TYPE_A, &[10, 0, 0, 1]);
        assert!(assemble(&parse_records(&packet)).is_empty());
    }

    #[test]
    fn truncated_and_query_packets_are_ignored() {
        assert!(parse_records(&[0x00, 0x00]).is_empty());
        let mut question = header(0, 0);
        question.extend_from_slice(&[0, 0, 0, 0]);
        question.extend_from_slice(&name(&["_airplay", "_tcp", "local"]));
        question.extend_from_slice(&[0x00, 0x0c, 0x00, 0x01]);
        assert!(parse_records(&question).is_empty(), "queries carry no answers");
        let mut truncated = header(0, 1);
        truncated.extend_from_slice(&[0xc0, 0x0c, 0x00]);
        assert!(parse_records(&truncated).is_empty());
    }

    #[test]
    fn txt_parsing_handles_equals_and_flags() {
        let mut packet = header(0, 1);
        record(
            &mut packet,
            &["room", "_airplay", "_tcp", "local"],
            TYPE_TXT,
            &[4, b'c', b'h', b'=', b'2', 4, b'g', b'v', b'=', b'0', 2, b's', b'f'],
        );
        let records = parse_records(&packet);
        match &records[0].data {
            RecordData::Txt(entries) => assert_eq!(entries, &["ch=2", "gv=0", "sf"]),
            other => panic!("unexpected record {other:?}"),
        }
        // Truncated character-strings are skipped, never panicked on.
        let mut packet = header(0, 1);
        record(
            &mut packet,
            &["room", "_airplay", "_tcp", "local"],
            TYPE_TXT,
            &[9, b'c', b'h', b'=', b'2'],
        );
        assert!(parse_records(&packet)[0]
            .data
            .eq(&RecordData::Txt(Vec::new())));
    }
}
