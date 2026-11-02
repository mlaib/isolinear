//! Packet captures: classic pcap (both byte orders, microsecond and nanosecond) and pcapng
//! (sections, interfaces, enhanced, simple and obsolete packet blocks). Packets are listed
//! with Ethernet (with VLAN tags), Linux cooked capture (v1 and v2), loopback and raw IP link
//! layers, IPv4, IPv6, ARP, TCP, UDP, ICMP, ICMPv6 and DNS decoded where possible. Decoding
//! stops after [`MAX_ROWS`] packets; the remaining packets are still counted.
use super::util::{Bytes, bytes_text, clip, span, thousands};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};

pub struct Pcap;

/// Classic pcap: (little-endian, nanoseconds).
fn classic(h: &[u8]) -> Option<(bool, bool)> {
    match h.get(..4)? {
        [0xd4, 0xc3, 0xb2, 0xa1] => Some((true, false)),
        [0xa1, 0xb2, 0xc3, 0xd4] => Some((false, false)),
        [0x4d, 0x3c, 0xb2, 0xa1] => Some((true, true)),
        [0xa1, 0xb2, 0x3c, 0x4d] => Some((false, true)),
        _ => None,
    }
}

fn pcapng(h: &[u8]) -> bool {
    h.starts_with(&[0x0a, 0x0d, 0x0d, 0x0a])
        && matches!(
            h.get(8..12),
            Some([0x4d, 0x3c, 0x2b, 0x1a]) | Some([0x1a, 0x2b, 0x3c, 0x4d])
        )
}

fn link_name(t: u32) -> String {
    match t {
        0 => "BSD loopback".into(),
        1 => "Ethernet".into(),
        101 => "raw IP".into(),
        105 => "IEEE 802.11".into(),
        108 => "OpenBSD loopback".into(),
        113 => "Linux cooked capture (SLL)".into(),
        127 => "802.11 with radiotap".into(),
        228 => "raw IPv4".into(),
        229 => "raw IPv6".into(),
        276 => "Linux cooked capture v2 (SLL2)".into(),
        t => format!("link type {t}"),
    }
}

/// What one packet decodes to.
#[derive(Default)]
struct Decoded {
    src: String,
    dst: String,
    proto: String,
    info: String,
}

fn mac(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}
fn v4(b: &[u8]) -> Option<Ipv4Addr> {
    Some(Ipv4Addr::new(
        *b.first()?,
        *b.get(1)?,
        *b.get(2)?,
        *b.get(3)?,
    ))
}
fn v6(b: &[u8]) -> Option<Ipv6Addr> {
    let a: [u8; 16] = b.get(..16)?.try_into().ok()?;
    Some(Ipv6Addr::from(a))
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn decode(link: u32, data: &[u8], le_host: bool) -> Decoded {
    let mut d = Decoded::default();
    match link {
        1 => ethernet(data, &mut d),
        113 => {
            if let Some(proto) = be16(data, 14) {
                ethertype(proto, data.get(16..).unwrap_or(&[]), &mut d);
            }
        }
        276 => {
            if let Some(proto) = be16(data, 0) {
                ethertype(proto, data.get(20..).unwrap_or(&[]), &mut d);
            }
        }
        0 | 108 => {
            let fam = data.get(..4).map(|f| {
                let a = [f[0], f[1], f[2], f[3]];
                if link == 108 || !le_host {
                    u32::from_be_bytes(a)
                } else {
                    u32::from_le_bytes(a)
                }
            });
            let rest = data.get(4..).unwrap_or(&[]);
            match fam {
                Some(2) => ipv4(rest, &mut d),
                Some(24) | Some(28) | Some(30) => ipv6(rest, &mut d),
                _ => d.proto = "loopback".into(),
            }
        }
        101 | 12 | 14 | 228 | 229 => match data.first().map(|b| b >> 4) {
            Some(4) => ipv4(data, &mut d),
            Some(6) => ipv6(data, &mut d),
            _ => d.proto = "IP?".into(),
        },
        t => d.proto = link_name(t),
    }
    d
}

fn ethernet(data: &[u8], d: &mut Decoded) {
    let (Some(dst), Some(src)) = (data.get(..6), data.get(6..12)) else {
        d.proto = "Ethernet (cut)".into();
        return;
    };
    d.src = mac(src);
    d.dst = mac(dst);
    let mut at = 12;
    let mut vlans = vec![];
    while let Some(t) = be16(data, at) {
        if (t == 0x8100 || t == 0x88a8) && vlans.len() < 2 {
            vlans.push(be16(data, at + 2).unwrap_or(0) & 0x0fff);
            at += 4;
            continue;
        }
        if t < 0x0600 {
            d.proto = "802.3/LLC".into();
            d.info = format!("length {t}");
        } else {
            ethertype(t, data.get(at + 2..).unwrap_or(&[]), d);
        }
        break;
    }
    if !vlans.is_empty() {
        let v: Vec<String> = vlans.iter().map(|v| v.to_string()).collect();
        d.info = format!("VLAN {} {}", v.join("/"), d.info).trim().to_owned();
    }
}

fn ethertype(t: u16, p: &[u8], d: &mut Decoded) {
    match t {
        0x0800 => ipv4(p, d),
        0x86dd => ipv6(p, d),
        0x0806 => arp(p, d),
        0x88cc => d.proto = "LLDP".into(),
        0x8863 | 0x8864 => d.proto = "PPPoE".into(),
        0x888e => d.proto = "EAPOL".into(),
        0x8847 | 0x8848 => d.proto = "MPLS".into(),
        t => d.proto = format!("EtherType 0x{t:04x}"),
    }
}

fn arp(p: &[u8], d: &mut Decoded) {
    d.proto = "ARP".into();
    let (Some(op), Some(hl), Some(pl)) = (be16(p, 6), p.get(4), p.get(5)) else {
        return;
    };
    let (hl, pl) = (*hl as usize, *pl as usize);
    if pl != 4 {
        return;
    }
    let sha = p.get(8..8 + hl).map(mac).unwrap_or_default();
    let spa = p.get(8 + hl..).and_then(v4);
    let tpa = p.get(8 + 2 * hl + pl..).and_then(v4);
    let (Some(spa), Some(tpa)) = (spa, tpa) else {
        return;
    };
    d.src = spa.to_string();
    d.dst = tpa.to_string();
    d.info = match op {
        1 => format!("Who has {tpa}? Tell {spa}"),
        2 => format!("{spa} is at {sha}"),
        o => format!("operation {o}"),
    };
}

fn ipv4(p: &[u8], d: &mut Decoded) {
    d.proto = "IPv4".into();
    let Some(&vi) = p.first() else {
        return;
    };
    let ihl = usize::from(vi & 0x0f) * 4;
    let (Some(total), Some(frag), Some(&proto), Some(src), Some(dst)) = (
        be16(p, 2),
        be16(p, 6),
        p.get(9),
        p.get(12..).and_then(v4),
        p.get(16..).and_then(v4),
    ) else {
        return;
    };
    d.src = src.to_string();
    d.dst = dst.to_string();
    if ihl < 20 {
        return;
    }
    let end = (total as usize).clamp(ihl, p.len().max(ihl));
    let payload = p.get(ihl..end.min(p.len())).unwrap_or(&[]);
    if frag & 0x1fff != 0 {
        d.info = format!(
            "fragment (protocol {proto}, offset {})",
            (frag & 0x1fff) * 8
        );
        return;
    }
    transport(proto, payload, d, false);
}

fn ipv6(p: &[u8], d: &mut Decoded) {
    d.proto = "IPv6".into();
    let (Some(plen), Some(&next), Some(src), Some(dst)) = (
        be16(p, 4),
        p.get(6),
        p.get(8..).and_then(v6),
        p.get(24..).and_then(v6),
    ) else {
        return;
    };
    d.src = src.to_string();
    d.dst = dst.to_string();
    let end = (40 + plen as usize).min(p.len());
    let mut payload = p.get(40..end).unwrap_or(&[]);
    let mut next = next;
    for _ in 0..8 {
        match next {
            0 | 43 | 60 => {
                let (Some(&n), Some(&l)) = (payload.first(), payload.get(1)) else {
                    return;
                };
                next = n;
                payload = payload.get((usize::from(l) + 1) * 8..).unwrap_or(&[]);
            }
            44 => {
                let Some(&n) = payload.first() else {
                    return;
                };
                if be16(payload, 2).unwrap_or(0) & 0xfff8 != 0 {
                    d.info = "fragment".into();
                    return;
                }
                next = n;
                payload = payload.get(8..).unwrap_or(&[]);
            }
            _ => break,
        }
    }
    transport(next, payload, d, true);
}

const TCP_FLAGS: [(u8, &str); 8] = [
    (0x02, "SYN"),
    (0x10, "ACK"),
    (0x01, "FIN"),
    (0x04, "RST"),
    (0x08, "PSH"),
    (0x20, "URG"),
    (0x40, "ECE"),
    (0x80, "CWR"),
];

fn transport(proto: u8, p: &[u8], d: &mut Decoded, is_v6: bool) {
    match proto {
        6 => {
            d.proto = "TCP".into();
            let (Some(sp), Some(dp), Some(seq), Some(ack), Some(&off), Some(&fl), Some(win)) = (
                be16(p, 0),
                be16(p, 2),
                be32(p, 4),
                be32(p, 8),
                p.get(12),
                p.get(13),
                be16(p, 14),
            ) else {
                d.info = "cut".into();
                return;
            };
            let hl = usize::from(off >> 4) * 4;
            let data = p.get(hl.max(20)..).unwrap_or(&[]);
            let f: Vec<&str> = TCP_FLAGS
                .iter()
                .filter(|(b, _)| fl & b != 0)
                .map(|(_, n)| *n)
                .collect();
            d.info = format!(
                "{sp} → {dp} [{}] Seq={seq} Ack={ack} Win={win} Len={}",
                f.join(", "),
                data.len()
            );
            if (sp == 53 || dp == 53) && data.len() > 2 {
                dns(&data[2..], d);
            }
            d.src = format!("{}:{sp}", bracket(&d.src, is_v6));
            d.dst = format!("{}:{dp}", bracket(&d.dst, is_v6));
        }
        17 => {
            d.proto = "UDP".into();
            let (Some(sp), Some(dp), Some(len)) = (be16(p, 0), be16(p, 2), be16(p, 4)) else {
                d.info = "cut".into();
                return;
            };
            d.info = format!("{sp} → {dp} Len={}", len.saturating_sub(8));
            let data = p.get(8..).unwrap_or(&[]);
            if matches!(sp, 53 | 5353 | 5355) || matches!(dp, 53 | 5353 | 5355) {
                dns(data, d);
            } else if matches!((sp, dp), (67, 68) | (68, 67)) {
                d.proto = "DHCP".into();
            } else if sp == 123 || dp == 123 {
                d.proto = "NTP".into();
            } else if sp == 443 || dp == 443 {
                d.proto = "UDP (QUIC?)".into();
            }
            d.src = format!("{}:{sp}", bracket(&d.src, is_v6));
            d.dst = format!("{}:{dp}", bracket(&d.dst, is_v6));
        }
        1 => {
            d.proto = "ICMP".into();
            let (Some(&t), Some(&c)) = (p.first(), p.get(1)) else {
                return;
            };
            let echo = || {
                format!(
                    "id={} seq={}",
                    be16(p, 4).unwrap_or(0),
                    be16(p, 6).unwrap_or(0)
                )
            };
            d.info = match t {
                0 => format!("Echo reply {}", echo()),
                8 => format!("Echo request {}", echo()),
                3 => format!("Destination unreachable (code {c})"),
                5 => "Redirect".into(),
                11 => "Time exceeded".into(),
                t => format!("type {t} code {c}"),
            };
        }
        58 => {
            d.proto = "ICMPv6".into();
            let (Some(&t), Some(&c)) = (p.first(), p.get(1)) else {
                return;
            };
            let echo = || {
                format!(
                    "id={} seq={}",
                    be16(p, 4).unwrap_or(0),
                    be16(p, 6).unwrap_or(0)
                )
            };
            let target = || {
                p.get(8..)
                    .and_then(v6)
                    .map(|a| a.to_string())
                    .unwrap_or_default()
            };
            d.info = match t {
                1 => format!("Destination unreachable (code {c})"),
                2 => "Packet too big".into(),
                3 => "Time exceeded".into(),
                128 => format!("Echo request {}", echo()),
                129 => format!("Echo reply {}", echo()),
                133 => "Router solicitation".into(),
                134 => "Router advertisement".into(),
                135 => format!("Neighbor solicitation for {}", target()),
                136 => format!("Neighbor advertisement {}", target()),
                143 => "Multicast listener report v2".into(),
                t => format!("type {t} code {c}"),
            };
        }
        2 => d.proto = "IGMP".into(),
        47 => d.proto = "GRE".into(),
        50 => d.proto = "ESP".into(),
        51 => d.proto = "AH".into(),
        132 => d.proto = "SCTP".into(),
        p => d.info = format!("protocol {p}"),
    }
}

fn bracket(a: &str, v6: bool) -> String {
    if v6 { format!("[{a}]") } else { a.to_owned() }
}

fn qtype(t: u16) -> String {
    match t {
        1 => "A",
        2 => "NS",
        5 => "CNAME",
        6 => "SOA",
        12 => "PTR",
        15 => "MX",
        16 => "TXT",
        28 => "AAAA",
        33 => "SRV",
        64 => "SVCB",
        65 => "HTTPS",
        255 => "ANY",
        _ => return format!("type {t}"),
    }
    .into()
}

/// A DNS name at `at` (with compression pointers, bounded): (name, offset after it).
fn dns_name(m: &[u8], at: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = vec![];
    let mut p = at;
    let mut after = None;
    for _ in 0..128 {
        let len = *m.get(p)?;
        if len == 0 {
            let name = if labels.is_empty() {
                ".".into()
            } else {
                labels.join(".")
            };
            return Some((clip(&name, 300), after.unwrap_or(p + 1)));
        }
        if len & 0xc0 == 0xc0 {
            let ptr = usize::from(be16(m, p)? & 0x3fff);
            after.get_or_insert(p + 2);
            p = ptr;
            continue;
        }
        let l = m.get(p + 1..p + 1 + usize::from(len))?;
        labels.push(
            l.iter()
                .map(|&c| if c.is_ascii_graphic() { c as char } else { '?' })
                .collect(),
        );
        p += 1 + usize::from(len);
    }
    None
}

fn dns(m: &[u8], d: &mut Decoded) {
    let (Some(id), Some(flags), Some(qd), Some(an)) =
        (be16(m, 0), be16(m, 2), be16(m, 4), be16(m, 6))
    else {
        return;
    };
    d.proto = "DNS".into();
    let response = flags & 0x8000 != 0;
    let mut text = format!(
        "Standard query{} 0x{id:04x}",
        if response { " response" } else { "" }
    );
    if flags & 0x7800 != 0 {
        text = format!("DNS opcode {} 0x{id:04x}", (flags >> 11) & 0xf);
    }
    let mut p = 12;
    for _ in 0..qd.min(4) {
        let Some((name, after)) = dns_name(m, p) else {
            break;
        };
        let t = be16(m, after).unwrap_or(0);
        text.push_str(&format!(" {} {name}", qtype(t)));
        p = after + 4;
    }
    let rcode = flags & 0xf;
    if response && rcode != 0 {
        text.push_str(match rcode {
            2 => " server failure",
            3 => " no such name",
            5 => " refused",
            _ => " error",
        });
    }
    for _ in 0..an.min(8) {
        let Some((_, after)) = dns_name(m, p) else {
            break;
        };
        let (Some(t), Some(len)) = (be16(m, after), be16(m, after + 8)) else {
            break;
        };
        let rd = after + 10;
        let value = match t {
            1 => m.get(rd..).and_then(v4).map(|a| a.to_string()),
            28 => m.get(rd..).and_then(v6).map(|a| a.to_string()),
            5 | 2 | 12 => dns_name(m, rd).map(|n| n.0),
            _ => None,
        };
        if let Some(v) = value {
            text.push_str(&format!(" {} {v}", qtype(t)));
        }
        p = rd + usize::from(len);
    }
    d.info = text;
}

/// A packet found in the file.
struct Packet<'a> {
    record: std::ops::Range<usize>,
    data: &'a [u8],
    orig: u64,
    /// Nanoseconds since the Unix epoch, when known.
    time: Option<i128>,
    link: u32,
}

struct Totals {
    count: u64,
    truncated: u64,
    first: Option<i128>,
    last: Option<i128>,
    protocols: BTreeMap<String, u64>,
}

fn fmt_time(ns: i128) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let frac = ns.rem_euclid(1_000_000_000);
    let base = crate::binary::utc(i64::try_from(secs).unwrap_or(0));
    // "2026-10-30 12:01 UTC" -> with seconds and microseconds.
    let s = secs.rem_euclid(60);
    base.replacen(" UTC", &format!(":{s:02}.{:06} UTC", frac / 1000), 1)
}

impl Handler for Pcap {
    fn name(&self) -> &'static str {
        "Packet capture"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        (classic(probe.head).is_some() && probe.head.len() >= 24) || pcapng(probe.head)
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        let mut out;
        let mut packets = Table {
            name: "Packets".into(),
            columns: [
                "No.",
                "Time",
                "Source",
                "Destination",
                "Protocol",
                "Length",
                "Info",
            ]
            .iter()
            .map(|c| (*c).to_owned())
            .collect(),
            ..Table::default()
        };
        let mut totals = Totals {
            count: 0,
            truncated: 0,
            first: None,
            last: None,
            protocols: BTreeMap::new(),
        };
        let mut damaged: Option<String> = None;
        let mut handle = |p: Packet, totals: &mut Totals, le_host: bool| {
            totals.count += 1;
            if (p.data.len() as u64) < p.orig {
                totals.truncated += 1;
            }
            if let Some(t) = p.time {
                totals.first.get_or_insert(t);
                totals.last = Some(t);
            }
            if packets.rows.len() >= MAX_ROWS {
                return;
            }
            let d = decode(p.link, p.data, le_host);
            *totals.protocols.entry(d.proto.clone()).or_default() += 1;
            let rel = match (p.time, totals.first) {
                (Some(t), Some(f)) => {
                    let us = (t - f) / 1000;
                    format!(
                        "{}.{:06}",
                        us.div_euclid(1_000_000),
                        us.rem_euclid(1_000_000)
                    )
                }
                _ => String::new(),
            };
            packets.rows.push(Row {
                cells: vec![
                    totals.count.to_string(),
                    rel,
                    d.src,
                    d.dst,
                    d.proto,
                    p.orig.to_string(),
                    d.info,
                ],
                source: span(p.record.start, p.record.end - p.record.start),
            });
        };
        if let Some((le, nanos)) = classic(b) {
            let r = Bytes::new(b, le);
            let (maj, min) = (r.u16(4).unwrap_or(0), r.u16(6).unwrap_or(0));
            let snap = r.u32(16).unwrap_or(0);
            let link = r.u32(20).unwrap_or(0) & 0x0fff_ffff;
            out = Inspection::new("Packet capture", "pcap capture");
            out.version = Some(format!("{maj}.{min}"));
            let hdr = span(0, 24);
            out.fact(
                "Capture",
                "Format",
                format!(
                    "pcap {maj}.{min}, {}, {} timestamps",
                    if le { "little-endian" } else { "big-endian" },
                    if nanos { "nanosecond" } else { "microsecond" }
                ),
                hdr.clone(),
            );
            out.fact("Capture", "Link type", link_name(link), hdr.clone());
            out.fact(
                "Capture",
                "Snapshot length",
                bytes_text(u64::from(snap)),
                hdr,
            );
            let mut pos = 24usize;
            while pos < b.len() {
                if totals.count.is_multiple_of(8192) && cancelled() {
                    return Err("Cancelled".into());
                }
                let (Some(sec), Some(frac), Some(incl), Some(orig)) =
                    (r.u32(pos), r.u32(pos + 4), r.u32(pos + 8), r.u32(pos + 12))
                else {
                    damaged = Some(format!("a packet header at byte {pos} is cut"));
                    break;
                };
                if incl > 0x1000_0000 {
                    damaged = Some(format!("the packet at byte {pos} claims {incl} bytes"));
                    break;
                }
                let Some(data) = r.get(pos + 16, incl as usize) else {
                    damaged = Some(format!("the packet at byte {pos} is cut"));
                    break;
                };
                let ns = i128::from(sec) * 1_000_000_000
                    + i128::from(frac) * if nanos { 1 } else { 1000 };
                let end = pos + 16 + incl as usize;
                handle(
                    Packet {
                        record: pos..end,
                        data,
                        orig: u64::from(orig),
                        time: Some(ns),
                        link,
                    },
                    &mut totals,
                    le,
                );
                pos = end;
            }
        } else if pcapng(b) {
            out = Inspection::new("Packet capture", "pcapng capture");
            let mut interfaces = Table {
                name: "Interfaces".into(),
                columns: [
                    "#",
                    "Section",
                    "Link type",
                    "Snapshot length",
                    "Name",
                    "Description",
                    "Time resolution",
                ]
                .iter()
                .map(|c| (*c).to_owned())
                .collect(),
                ..Table::default()
            };
            // Per section: interfaces (link type, units per second).
            let mut ifs: Vec<(u32, u128)> = vec![];
            let mut sections = 0u32;
            let mut le = true;
            let mut pos = 0usize;
            let mut secrets = 0;
            while pos < b.len() {
                if totals.count.is_multiple_of(8192) && cancelled() {
                    return Err("Cancelled".into());
                }
                if b.get(pos..pos + 4) == Some(&[0x0a, 0x0d, 0x0d, 0x0a]) {
                    match b.get(pos + 8..pos + 12) {
                        Some([0x4d, 0x3c, 0x2b, 0x1a]) => le = true,
                        Some([0x1a, 0x2b, 0x3c, 0x4d]) => le = false,
                        _ => {
                            damaged = Some(format!(
                                "a section header at byte {pos} has no byte-order magic"
                            ));
                            break;
                        }
                    }
                }
                let r = Bytes::new(b, le);
                let (Some(ty), Some(len)) = (r.u32(pos), r.u32(pos + 4)) else {
                    damaged = Some(format!("a block header at byte {pos} is cut"));
                    break;
                };
                let len = len as usize;
                if len < 12
                    || !len.is_multiple_of(4)
                    || pos.checked_add(len).is_none_or(|e| e > b.len())
                {
                    damaged = Some(format!("the block at byte {pos} has a bad length ({len})"));
                    break;
                }
                let body = pos + 8;
                let body_end = pos + len - 4;
                let options = |mut at: usize| -> Vec<(u16, &[u8])> {
                    let mut v = vec![];
                    while at + 4 <= body_end && v.len() < 256 {
                        let (Some(code), Some(l)) = (r.u16(at), r.u16(at + 2)) else {
                            break;
                        };
                        if code == 0 {
                            break;
                        }
                        let Some(val) = r
                            .get(at + 4, l as usize)
                            .filter(|_| at + 4 + l as usize <= body_end)
                        else {
                            break;
                        };
                        v.push((code, val));
                        at += 4 + (l as usize).div_ceil(4) * 4;
                    }
                    v
                };
                let text =
                    |v: &[u8]| clip(&super::util::printable(&String::from_utf8_lossy(v)), 300);
                match ty {
                    0x0a0d_0d0a => {
                        sections += 1;
                        ifs.clear();
                        let (maj, min) =
                            (r.u16(body + 4).unwrap_or(0), r.u16(body + 6).unwrap_or(0));
                        if sections == 1 {
                            out.version = Some(format!("{maj}.{min}"));
                            out.fact(
                                "Capture",
                                "Format",
                                format!(
                                    "pcapng {maj}.{min}, {}",
                                    if le { "little-endian" } else { "big-endian" }
                                ),
                                span(pos, len),
                            );
                        }
                        for (code, v) in options(body + 16) {
                            let label = match code {
                                2 => "Hardware",
                                3 => "Operating system",
                                4 => "Capture application",
                                1 => "Comment",
                                _ => continue,
                            };
                            out.fact("Capture", label, text(v), span(pos, len));
                        }
                    }
                    1 => {
                        let link = u32::from(r.u16(body).unwrap_or(0));
                        let snap = r.u32(body + 4).unwrap_or(0);
                        let mut name = String::new();
                        let mut desc = String::new();
                        let mut ups: u128 = 1_000_000;
                        for (code, v) in options(body + 8) {
                            match code {
                                2 => name = text(v),
                                3 => desc = text(v),
                                9 => {
                                    let t = v.first().copied().unwrap_or(6);
                                    ups = if t & 0x80 != 0 {
                                        1u128.checked_shl(u32::from(t & 0x7f)).unwrap_or(1)
                                    } else {
                                        10u128.checked_pow(u32::from(t)).unwrap_or(1_000_000)
                                    };
                                }
                                _ => {}
                            }
                        }
                        ifs.push((link, ups.max(1)));
                        if interfaces.rows.len() < MAX_ROWS {
                            interfaces.rows.push(Row {
                                cells: vec![
                                    (ifs.len() - 1).to_string(),
                                    sections.to_string(),
                                    link_name(link),
                                    bytes_text(u64::from(snap)),
                                    name,
                                    desc,
                                    format!(
                                        "1/{} s",
                                        thousands(u64::try_from(ups).unwrap_or(u64::MAX))
                                    ),
                                ],
                                source: span(pos, len),
                            });
                        }
                    }
                    6 | 2 => {
                        let (iface, hi, lo, cap, orig, data_at) = if ty == 6 {
                            (
                                r.u32(body),
                                r.u32(body + 4),
                                r.u32(body + 8),
                                r.u32(body + 12),
                                r.u32(body + 16),
                                body + 20,
                            )
                        } else {
                            (
                                r.u16(body).map(u32::from),
                                r.u32(body + 4),
                                r.u32(body + 8),
                                r.u32(body + 12),
                                r.u32(body + 16),
                                body + 20,
                            )
                        };
                        let (Some(iface), Some(hi), Some(lo), Some(cap), Some(orig)) =
                            (iface, hi, lo, cap, orig)
                        else {
                            damaged = Some(format!("the packet block at byte {pos} is cut"));
                            break;
                        };
                        let Some(data) = r
                            .get(data_at, cap as usize)
                            .filter(|_| data_at + cap as usize <= body_end)
                        else {
                            damaged = Some(format!(
                                "the packet block at byte {pos} claims more data than it holds"
                            ));
                            break;
                        };
                        let (link, ups) = ifs
                            .get(iface as usize)
                            .copied()
                            .unwrap_or((u32::MAX, 1_000_000));
                        let ts = (u128::from(hi) << 32) | u128::from(lo);
                        let ns = ts
                            .checked_mul(1_000_000_000)
                            .map(|v| v / ups)
                            .and_then(|v| i128::try_from(v).ok());
                        handle(
                            Packet {
                                record: pos..pos + len,
                                data,
                                orig: u64::from(orig),
                                time: ns,
                                link,
                            },
                            &mut totals,
                            le,
                        );
                    }
                    3 => {
                        let orig = r.u32(body).unwrap_or(0);
                        let avail = body_end.saturating_sub(body + 4);
                        let data = r.get(body + 4, avail.min(orig as usize)).unwrap_or(&[]);
                        let link = ifs.first().map(|i| i.0).unwrap_or(u32::MAX);
                        handle(
                            Packet {
                                record: pos..pos + len,
                                data,
                                orig: u64::from(orig),
                                time: None,
                                link,
                            },
                            &mut totals,
                            le,
                        );
                    }
                    0x0000_000a => secrets += 1,
                    _ => {}
                }
                pos += len;
            }
            out.fact("Capture", "Sections", sections.to_string(), None);
            out.fact(
                "Capture",
                "Interfaces",
                interfaces.rows.len().to_string(),
                None,
            );
            let links: Vec<String> = interfaces
                .rows
                .iter()
                .map(|r| r.cells[2].clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            out.fact("Capture", "Link type", links.join(", "), None);
            if secrets > 0 {
                out.fact(
                    "Capture",
                    "Decryption secrets blocks",
                    format!("{secrets} (contents not shown)"),
                    None,
                );
                out.notes.push("The capture holds decryption secrets (for example TLS session keys); Isolinear does not show them.".into());
            }
            if !interfaces.rows.is_empty() {
                out.tables.push(interfaces);
            }
        } else {
            return Err("Not a packet capture".into());
        }
        out.fact("Packets", "Packets", thousands(totals.count), None);
        if let (Some(f), Some(l)) = (totals.first, totals.last) {
            out.fact("Packets", "First packet", fmt_time(f), None);
            out.fact("Packets", "Last packet", fmt_time(l), None);
            let us = (l - f) / 1000;
            out.fact(
                "Packets",
                "Time span",
                format!(
                    "{}.{:06} s",
                    us.div_euclid(1_000_000),
                    us.rem_euclid(1_000_000)
                ),
                None,
            );
        }
        if totals.truncated > 0 {
            out.fact(
                "Packets",
                "Cut by the snapshot length",
                thousands(totals.truncated),
                None,
            );
        }
        let mut protos: Vec<(String, u64)> = totals.protocols.into_iter().collect();
        protos.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.fact(
            "Packets",
            "Protocols",
            protos
                .iter()
                .map(|(p, n)| format!("{p} {n}"))
                .collect::<Vec<_>>()
                .join(", "),
            None,
        );
        if totals.count > packets.rows.len() as u64 {
            packets.total = Some(totals.count);
            out.notes.push(format!(
                "Only the first {} packets are decoded; all {} were counted.",
                thousands(MAX_ROWS as u64),
                thousands(totals.count)
            ));
        }
        if !packets.rows.is_empty() {
            out.tables.push(packets);
        }
        match damaged {
            None => out.check(
                Stage::Parsed,
                State::Passed,
                "Every packet record was read; protocols decoded where recognized",
            ),
            Some(e) => out.check(
                Stage::Parsed,
                State::Partial,
                format!("Read up to damage: {e}"),
            ),
        }
        out.not_validated();
        out.notes
            .push("Packets are decoded from their bytes only; nothing is sent or replayed.".into());
        Ok(out)
    }
}
