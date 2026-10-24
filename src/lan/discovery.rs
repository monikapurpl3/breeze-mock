//! Answering discovery, so `breeze-core pair` and scans find emulated units.
//!
//! Each unit answers a unicast hello on its own address, which is how Breeze
//! Core's scan sweeps a subnet; one wildcard socket per port also answers the
//! broadcast hello, replying from each unit's own address, as real units do.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use breeze_proto::discover::{DISCOVERY_MSG, DISCOVERY_PORTS};
use breeze_proto::security;
use socket2::{Domain, Protocol, Socket, Type};

use crate::blueprint::{ProtocolVersion, Unit};
use crate::sim::Shared;
use crate::{debug, error, info};

/// The reply a unit sends to a hello: its identity, encrypted the way the
/// vendor's apps expect, in a V2 or V3 envelope.
pub fn reply(unit: &Unit) -> Vec<u8> {
    let o = unit.ip.octets();
    let mut plain = vec![o[3], o[2], o[1], o[0]];
    plain.extend_from_slice(&unit.protocol.port.to_le_bytes());
    plain.extend_from_slice(&[0, 0]);
    let mut serial = unit.identity.serial.as_bytes().to_vec();
    serial.resize(32, b'0');
    plain.extend_from_slice(&serial);
    plain.push(unit.identity.ssid.len() as u8);
    plain.extend_from_slice(unit.identity.ssid.as_bytes());
    // The vendor pads the plaintext with further fields nobody reads.
    plain.resize(plain.len().max(64), 0);
    let encrypted = security::encrypt_aes(&plain);

    let mut body = vec![0x5A, 0x5A, 0x01, 0x11, 0, 0, 0x20, 0x00];
    body.resize(20, 0);
    body.extend_from_slice(&unit.id.to_le_bytes()[..6]);
    body.resize(40, 0);
    body.extend_from_slice(&encrypted);
    let total = (body.len() + 16) as u16;
    body[4..6].copy_from_slice(&total.to_le_bytes());
    let digest = security::sign(&body);
    body.extend_from_slice(&digest);

    match unit.protocol.version {
        ProtocolVersion::V2 => body,
        ProtocolVersion::V3 => {
            let mut v3 = vec![0x83, 0x70];
            v3.extend_from_slice(&((body.len() + 16) as u16).to_be_bytes());
            v3.extend_from_slice(&[0x20, 0x00, 0x00, 0x00]);
            v3.extend_from_slice(&body);
            v3.extend_from_slice(&security::sign(&v3)[..16]);
            v3
        }
    }
}

fn reusable_udp(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    s.set_broadcast(true)?;
    s.bind(&addr.into())?;
    Ok(s.into())
}

/// Start answering hellos for the given units (indices into the world).
///
/// One socket per address and port, answering for every unit at that address:
/// units may share one (on Windows they share this PC's LAN address, one port
/// each), and a datagram is delivered to only one of several sockets bound to
/// the same address. Each unit answers with its own id and port, and Breeze
/// Core's scan keeps one result per id, so all of them are found.
pub fn start(world: Shared, units: Vec<usize>) {
    let mut by_ip: BTreeMap<Ipv4Addr, Vec<(usize, Unit)>> = BTreeMap::new();
    {
        let Ok(w) = world.lock() else { return };
        for &i in &units {
            let u = w.units[i].def.clone();
            by_ip.entry(u.ip).or_default().push((i, u));
        }
    }
    for port in DISCOVERY_PORTS {
        let mut groups: Vec<Group> = Vec::new();
        for (ip, members) in &by_ip {
            let addr = SocketAddr::new(IpAddr::V4(*ip), port);
            match reusable_udp(addr) {
                Ok(sock) => {
                    if let Ok(clone) = sock.try_clone() {
                        groups.push((*ip, members.clone(), clone));
                    }
                    let (world, members) = (world.clone(), members.clone());
                    std::thread::spawn(move || answer(&world, &members, &sock));
                }
                Err(e) => error!("discovery", "cannot answer hellos on {addr}: {e}"),
            }
        }
        // Broadcast hellos arrive on a wildcard socket. Every unit the asker
        // can reach answers, from its own address, as a real unit's reply
        // would come; a loopback unit cannot answer a hello from the LAN.
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
        match reusable_udp(addr) {
            Ok(sock) => {
                let world = world.clone();
                std::thread::spawn(move || broadcast(&world, &sock, &groups));
            }
            Err(e) => error!(
                "discovery",
                "cannot listen for broadcast hellos on {addr}: {e}"
            ),
        }
    }
}

/// The units at one address, and the socket that answers for them there.
type Group = (Ipv4Addr, Vec<(usize, Unit)>, UdpSocket);

fn is_hello(buf: &[u8]) -> bool {
    buf == DISCOVERY_MSG
}

fn answer(world: &Shared, members: &[(usize, Unit)], sock: &UdpSocket) {
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf) else {
            continue;
        };
        if !is_hello(&buf[..n]) {
            debug!(
                "discovery",
                "ignoring a {n}-byte datagram from {from} that is not a discovery hello"
            );
            continue;
        }
        for (i, u) in members {
            send(world, *i, u, sock, from);
        }
    }
}

fn reachable(unit_ip: Ipv4Addr, from: SocketAddr) -> bool {
    let from_loop = match from.ip() {
        IpAddr::V4(a) => a.is_loopback(),
        IpAddr::V6(a) => a.is_loopback(),
    };
    unit_ip.is_loopback() == from_loop
}

fn broadcast(world: &Shared, sock: &UdpSocket, groups: &[Group]) {
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf) else {
            continue;
        };
        if !is_hello(&buf[..n]) {
            continue;
        }
        for (ip, members, s) in groups {
            if !reachable(*ip, from) {
                continue;
            }
            for (i, u) in members {
                send(world, *i, u, s, from);
            }
        }
    }
}

fn send(world: &Shared, i: usize, u: &Unit, sock: &UdpSocket, to: SocketAddr) {
    if world.lock().map(|w| w.offline(i)).unwrap_or(false) {
        debug!(u.key, "offline: not answering a hello from {to}");
        return;
    }
    match sock.send_to(&reply(u), to) {
        Ok(_) => {
            info!(u.key, "answered a discovery hello from {to}");
            if let Ok(mut w) = world.lock() {
                w.units[i].stats.discovery_replies += 1;
            }
        }
        Err(e) => error!(u.key, "cannot answer {to}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use breeze_proto::discover::{self, Version};

    #[test]
    fn breeze_proto_reads_a_unit_from_its_reply() {
        let path = std::env::temp_dir().join(format!("bm-disc-{}.xml", std::process::id()));
        std::fs::write(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<blueprint xmlns="urn:breeze-mock:blueprint:1" version="1" name="t">
  <network><segment name="home" cidr="10.77.1.0/24" mode="existing"/></network>
  <unit key="u" id="153931628470980" segment="home" ip="10.77.1.21">
    <epithets name="U"/>
    <identity serial="000000P0000000Q1AC72DD0A23E40000" ssid="net_ac_23E4"/>
    <v3 port="6444" token="auto" key="auto"/>
  </unit>
  <unit key="old" segment="home" ip="10.77.1.22">
    <epithets name="Old"/>
    <v2/>
  </unit>
</blueprint>
"#,
        )
        .unwrap();
        let bp = crate::blueprint::load(&path).unwrap();
        let d = discover::parse(bp.units[0].ip, &super::reply(&bp.units[0])).unwrap();
        assert_eq!(d.version, Version::V3);
        assert_eq!(d.id, 153_931_628_470_980);
        assert_eq!(d.reported_ip, bp.units[0].ip);
        assert_eq!(d.port, 6444);
        assert_eq!(d.ssid, "net_ac_23E4");
        assert_eq!(d.device_type, 0xAC);
        assert_eq!(d.serial_number, "000000P0000000Q1AC72DD0A23E40000");
        let old = discover::parse(bp.units[1].ip, &super::reply(&bp.units[1])).unwrap();
        assert_eq!(old.version, Version::V2);
        assert_eq!(old.id, bp.units[1].id);
    }
}
