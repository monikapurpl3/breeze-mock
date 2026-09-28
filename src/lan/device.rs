//! One emulated unit on the network: a TCP listener speaking the Midea LAN
//! protocol, V3 (token handshake, AES-256 session) or V2 (plain packets).
//!
//! Everything a unit does wrong lives here or in the simulation, and each is
//! something a real unit was measured doing: answering after ~0.72 s,
//! swallowing about one request in thirty while the connection stays usable,
//! ignoring the first request after a handshake, hanging up after 30 s idle,
//! and going quiet at times of day.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use breeze_proto::lan::{self, PacketType};
use breeze_proto::{frame, packet, security};
use sha2::Digest as _;

use super::codec;
use crate::blueprint::ProtocolVersion;
use crate::sim::rng::Rng;
use crate::sim::Shared;
use crate::{debug, info};

/// Bind a unit's listener and serve it until the process ends. Returns the
/// address actually bound, which differs from `bind` when it asked for port 0.
pub fn start(world: Shared, index: usize, bind: SocketAddr) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind(bind)?;
    let addr = listener.local_addr()?;
    let who = world
        .lock()
        .map(|w| w.units[index].def.key.clone())
        .unwrap_or_default();
    info!(who, "listening on {addr}");
    std::thread::spawn(move || {
        let mut n = 0u64;
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            n += 1;
            let world = world.clone();
            let who = who.clone();
            std::thread::spawn(move || {
                let peer = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                info!(who, "connection from {peer}");
                if let Err(e) = serve(&world, index, stream, n) {
                    debug!(who, "connection from {peer} ended: {e}");
                } else {
                    info!(who, "connection from {peer} closed");
                }
            });
        }
    });
    Ok(addr)
}

/// What one connection needs to know, copied out of the world once.
struct Unit {
    key: String,
    id: u64,
    version: ProtocolVersion,
    token: Vec<u8>,
    secret: [u8; 32],
    behaviour: crate::blueprint::Behaviour,
    pages: Vec<Vec<u8>>,
}

fn serve(world: &Shared, index: usize, mut stream: TcpStream, conn: u64) -> io::Result<()> {
    let unit = {
        let mut w = world
            .lock()
            .map_err(|_| io::Error::other("world poisoned"))?;
        let u = &mut w.units[index];
        u.stats.connections += 1;
        Unit {
            key: u.def.key.clone(),
            id: u.def.id,
            version: u.def.protocol.version,
            token: u.def.protocol.token.clone(),
            secret: u.def.protocol.key,
            behaviour: u.def.behaviour.clone(),
            pages: codec::capability_pages(&u.def.capabilities).map_err(io::Error::other)?,
        }
    };
    let seed = world.lock().map(|w| w.rng.clone().next_u64()).unwrap_or(0);
    let mut rng = Rng::new(seed ^ unit.id ^ conn.rotate_left(32));
    stream.set_nodelay(true)?;

    let mut session: Option<([u8; 32], Instant)> = None;
    let mut rx: Vec<u8> = Vec::new();
    let mut buf = [0u8; 2048];
    let mut last_request = Instant::now();
    // Short reads, so the idle timer and the session lifetime are checked
    // even while nothing arrives.
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => rx.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
        if let Some(idle) = unit.behaviour.idle_hangup {
            if last_request.elapsed() >= idle {
                info!(
                    unit.key,
                    "hanging up after {:?} idle, as real units do", idle
                );
                return Ok(());
            }
        }
        if let (Some(life), Some((_, since))) = (unit.behaviour.session_lifetime, session) {
            if since.elapsed() >= life {
                info!(
                    unit.key,
                    "session is {:?} old; forgetting it and hanging up", life
                );
                return Ok(());
            }
        }
        while let Some(total) = packet_len(&rx) {
            if rx.len() < total {
                break;
            }
            let pkt: Vec<u8> = rx.drain(..total).collect();
            last_request = Instant::now();
            if world.lock().map(|w| w.offline(index)).unwrap_or(false) {
                debug!(unit.key, "offline: not answering");
                continue;
            }
            if let Some(reply) = handle(world, index, &unit, &pkt, &mut session, &mut rng)? {
                stream.write_all(&reply)?;
            }
        }
        if rx.len() > 64 * 1024 {
            return Err(io::Error::other("64 KiB without a complete packet"));
        }
    }
}

/// The length of the packet at the start of `rx`, once its header is in.
fn packet_len(rx: &[u8]) -> Option<usize> {
    match rx {
        [0x83, 0x70, ..] => lan::packet_len(rx),
        [0x5A, 0x5A, _, _, lo, hi, ..] => Some(u16::from_le_bytes([*lo, *hi]) as usize),
        [_, _, ..] => Some(rx.len()), // garbage: consume it and let handle refuse it
        _ => None,
    }
}

/// An explicit error packet, which the client reads as "the device refused".
fn error_packet() -> Vec<u8> {
    vec![
        0x83,
        0x70,
        0x00,
        0x00,
        0x20,
        PacketType::Error as u8,
        0x00,
        0x00,
    ]
}

fn handle(
    world: &Shared,
    index: usize,
    unit: &Unit,
    pkt: &[u8],
    session: &mut Option<([u8; 32], Instant)>,
    rng: &mut Rng,
) -> io::Result<Option<Vec<u8>>> {
    let v2 = match (unit.version, pkt) {
        (ProtocolVersion::V3, [0x83, 0x70, ..]) => match pkt[5] & 0xF {
            t if t == PacketType::HandshakeRequest as u8 => {
                return Ok(Some(handshake(world, index, unit, pkt, session, rng)));
            }
            t if t == PacketType::EncryptedRequest as u8 => {
                let Some((key, _)) = session else {
                    info!(unit.key, "encrypted request before any handshake; refusing");
                    return Ok(Some(error_packet()));
                };
                lan::decode_encrypted(pkt, key)
                    .map_err(|e| io::Error::other(e.to_string()))?
                    .1
            }
            other => {
                return Err(io::Error::other(format!(
                    "unexpected V3 packet type {other}"
                )))
            }
        },
        (ProtocolVersion::V2, [0x5A, 0x5A, ..]) => pkt.to_vec(),
        (v, _) => {
            return Err(io::Error::other(format!(
                "not a {} packet (starts {:02X?})",
                if v == ProtocolVersion::V3 { "V3" } else { "V2" },
                &pkt[..pkt.len().min(4)]
            )))
        }
    };

    let framed = packet::decode(&v2).map_err(io::Error::other)?;
    let frame_type = framed
        .get(9)
        .copied()
        .unwrap_or(frame::FrameType::Report as u8);
    let payload =
        frame::parse(&framed, frame::DeviceType::AirConditioner).map_err(io::Error::other)?;
    let b = &unit.behaviour;

    if let Some((_, since)) = session {
        if since.elapsed() < b.post_handshake_deafness {
            info!(
                unit.key,
                "request {:?} after the handshake: not listening yet",
                since.elapsed()
            );
            count_drop(world, index);
            return Ok(None);
        }
    }
    if rng.chance(b.drop_rate) {
        info!(
            unit.key,
            "swallowing a 0x{:02X} request without answering",
            payload.first().copied().unwrap_or(0)
        );
        count_drop(world, index);
        return Ok(None);
    }

    let Some(reply) = respond(world, index, unit, payload)? else {
        return Ok(None);
    };
    let delay = b.reply_delay + Duration::from_secs_f64(b.jitter.as_secs_f64() * rng.unit());
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
    let ftype = match frame_type {
        0x02 => frame::FrameType::Control,
        0x03 => frame::FrameType::Query,
        _ => frame::FrameType::Report,
    };
    let framed = frame::build(frame::DeviceType::AirConditioner, ftype, &reply);
    let v2 = packet::encode(unit.id, &framed);
    Ok(Some(match session {
        Some((key, _)) => lan::encode_encrypted(key, 0, &v2, 0, PacketType::EncryptedResponse)
            .map_err(|e| io::Error::other(e.to_string()))?,
        None => v2,
    }))
}

fn count_drop(world: &Shared, index: usize) {
    if let Ok(mut w) = world.lock() {
        w.units[index].stats.dropped += 1;
    }
}

fn handshake(
    world: &Shared,
    index: usize,
    unit: &Unit,
    pkt: &[u8],
    session: &mut Option<([u8; 32], Instant)>,
    rng: &mut Rng,
) -> Vec<u8> {
    let token = pkt.get(8..).unwrap_or(&[]);
    let refuse = unit.behaviour.reject_handshake || token != unit.token.as_slice();
    if let Ok(mut w) = world.lock() {
        let s = &mut w.units[index].stats;
        if refuse {
            s.handshakes_refused += 1;
        } else {
            s.handshakes += 1;
        }
    }
    if refuse {
        info!(
            unit.key,
            "refusing a handshake ({})",
            if unit.behaviour.reject_handshake {
                "estranged"
            } else {
                "the token is not this unit's"
            }
        );
        *session = None;
        return error_packet();
    }
    // A real unit picks these at random; so does this, from the seeded rng.
    let plain: [u8; 32] = rng.bytes();
    let mut payload =
        security::encrypt_aes_cbc(&unit.secret, &plain).expect("32 bytes is block aligned");
    payload.extend_from_slice(&sha2::Sha256::digest(plain));
    let mut key = [0u8; 32];
    for i in 0..32 {
        key[i] = plain[i] ^ unit.secret[i];
    }
    *session = Some((key, Instant::now()));
    info!(unit.key, "handshake accepted");
    let mut reply = vec![0x83, 0x70];
    reply.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    reply.push(0x20);
    reply.push(PacketType::HandshakeResponse as u8);
    reply.extend_from_slice(pkt.get(6..8).unwrap_or(&[0, 0]));
    reply.extend_from_slice(&payload);
    reply
}

/// The appliance's answer to one payload, or `None` for a request it does
/// not understand -- which a real unit simply ignores.
fn respond(
    world: &Shared,
    index: usize,
    unit: &Unit,
    payload: &[u8],
) -> io::Result<Option<Vec<u8>>> {
    let mut w = world
        .lock()
        .map_err(|_| io::Error::other("world poisoned"))?;
    match payload.first() {
        Some(0x41) => {
            w.units[index].stats.queries += 1;
            let r = w.report(index);
            debug!(
                unit.key,
                "state query answered: power {} {:?} {}", r.power, r.mode, r.target
            );
            Ok(Some(codec::encode_report(&r)))
        }
        Some(0x40) => {
            let cmd = codec::decode_command(payload).map_err(io::Error::other)?;
            let (r, refused) = w.control(index, &cmd);
            if refused.is_empty() {
                info!(
                    unit.key,
                    "control applied: power {} {:?} {} fan {} swing {:?}",
                    r.power,
                    r.mode,
                    r.target,
                    r.fan,
                    r.swing
                );
            } else {
                info!(
                    unit.key,
                    "control applied, but kept its own {}",
                    refused.join(", ")
                );
            }
            Ok(Some(codec::encode_report(&r)))
        }
        Some(0xB5) => {
            w.units[index].stats.capability_queries += 1;
            let page = codec::capability_page(payload).unwrap_or(0);
            Ok(Some(match unit.pages.get(page) {
                Some(p) => p.clone(),
                // Asked for a second page it never offered: an empty one.
                None => vec![0xB5, 0x00, 0x00, 0x00],
            }))
        }
        other => {
            info!(unit.key, "ignoring a request with opcode {other:02X?}");
            Ok(None)
        }
    }
}
