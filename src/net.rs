//! Putting the units' addresses on this machine for a run, and taking them off
//! again afterwards.
//!
//! - `loopback`: nothing to do. Linux and Windows both answer every address in
//!   127.0.0.0/8 without configuration.
//! - `existing`: nothing to do; the addresses must already be this machine's.
//!   On Windows this is how units reach other machines: they share this PC's
//!   LAN address, one port each.
//! - `dummy` (Linux): a dummy interface made for the run, carrying the
//!   segment's addresses, removed afterwards.
//! - `attach` (Linux): extra addresses on an existing interface, so other
//!   machines on that LAN reach the units; labelled `<if>:bm` so a run that
//!   died without cleaning up is cleaned up by the next one.
//!
//! Every address is then bound once, on every platform, so a missing one is
//! reported before anything starts listening -- not as one unit silently
//! absent from a scan.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Mutex;

use crate::blueprint::{Blueprint, Segment, SegmentMode};
use crate::info;

/// What to undo when the run ends. Released once, by `release` or on drop.
pub struct Guard {
    undo: Mutex<Vec<Vec<String>>>,
}

impl Guard {
    fn new() -> Self {
        Self {
            undo: Mutex::new(Vec::new()),
        }
    }

    /// Undo everything, newest first. Safe to call more than once.
    pub fn release(&self) {
        let steps = match self.undo.lock() {
            Ok(mut u) => std::mem::take(&mut *u),
            Err(_) => return,
        };
        for args in steps.iter().rev() {
            match ip(args) {
                Ok(_) => info!("network", "undone: ip {}", args.join(" ")),
                Err(e) => crate::error!("network", "could not undo `ip {}`: {e}", args.join(" ")),
            }
        }
    }

    fn push(&self, args: Vec<String>) {
        if let Ok(mut u) = self.undo.lock() {
            u.push(args);
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.release();
    }
}

/// The distinct unit addresses on one segment.
fn addresses(bp: &Blueprint, seg: &Segment) -> BTreeSet<Ipv4Addr> {
    bp.units
        .iter()
        .filter(|u| u.segment == seg.name)
        .map(|u| u.ip)
        .collect()
}

/// Make every unit's address usable, or say exactly why it cannot be.
pub fn setup(bp: &Blueprint) -> Result<Guard, String> {
    let guard = Guard::new();
    for seg in &bp.segments {
        let ips = addresses(bp, seg);
        if ips.is_empty() {
            continue;
        }
        match &seg.mode {
            SegmentMode::Loopback | SegmentMode::Existing => {}
            SegmentMode::Dummy { interface } => {
                linux_only(seg)?;
                dummy(&guard, seg, interface, &ips)?;
            }
            SegmentMode::Attach { interface } => {
                linux_only(seg)?;
                attach(&guard, seg, interface, &ips)?;
            }
        }
    }
    check_bindable(bp)?;
    Ok(guard)
}

fn linux_only(seg: &Segment) -> Result<(), String> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err(format!(
            "segment {:?} is mode=\"{}\", which makes network interfaces and only works on Linux. \
             Here, use mode=\"loopback\" (127.x addresses, this machine only) or \
             mode=\"existing\" with an address this machine already has, giving each unit its own port",
            seg.name,
            if matches!(seg.mode, SegmentMode::Dummy { .. }) { "dummy" } else { "attach" }
        ))
    }
}

/// Run `ip` with these arguments; its stderr on failure.
fn ip(args: &[String]) -> Result<String, String> {
    let out = std::process::Command::new("ip")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run ip: {e} (it is in iproute2)"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn s(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

fn privileged_hint(e: String) -> String {
    if e.contains("Operation not permitted") {
        format!("{e} -- this needs root or CAP_NET_ADMIN (the packaged service has it)")
    } else {
        e
    }
}

fn dummy(guard: &Guard, seg: &Segment, interface: &str, ips: &BTreeSet<Ipv4Addr>) -> Result<(), String> {
    // A dummy interface of this name left by a run that died: ours to remove.
    if let Ok(detail) = ip(&s(&["-d", "link", "show", "dev", interface])) {
        if detail.contains("dummy") {
            info!("network", "removing {interface}, left over from an earlier run");
            ip(&s(&["link", "del", interface])).map_err(privileged_hint)?;
        } else {
            return Err(format!(
                "segment {:?}: interface {interface} exists and is not a dummy; \
                 choose another interface= for it",
                seg.name
            ));
        }
    }
    ip(&s(&["link", "add", interface, "type", "dummy"])).map_err(privileged_hint)?;
    guard.push(s(&["link", "del", interface]));
    ip(&s(&["link", "set", interface, "up"])).map_err(privileged_hint)?;
    for addr in ips {
        let cidr = format!("{addr}/{}", seg.prefix);
        ip(&s(&["addr", "add", &cidr, "dev", interface])).map_err(privileged_hint)?;
    }
    info!(
        "network",
        "segment {}: dummy interface {interface} with {} address(es) in {}",
        seg.name,
        ips.len(),
        seg.cidr()
    );
    Ok(())
}

fn attach(guard: &Guard, seg: &Segment, interface: &str, ips: &BTreeSet<Ipv4Addr>) -> Result<(), String> {
    ip(&s(&["link", "show", "dev", interface]))
        .map_err(|_| format!("segment {:?}: there is no interface {interface}", seg.name))?;
    // Linux labels are at most 15 characters and must start with the name.
    let label = format!("{interface}:bm");
    let labelled = label.len() <= 15;
    if labelled {
        // Addresses a run that died left behind carry our label.
        let shown = ip(&s(&["-o", "-4", "addr", "show", "dev", interface, "label", &label]))
            .unwrap_or_default();
        for line in shown.lines() {
            if let Some(cidr) = line.split_whitespace().skip_while(|w| *w != "inet").nth(1) {
                info!("network", "removing {cidr} from {interface}, left over from an earlier run");
                ip(&s(&["addr", "del", cidr, "dev", interface])).map_err(privileged_hint)?;
            }
        }
    }
    let present = ip(&s(&["-o", "-4", "addr", "show", "dev", interface])).unwrap_or_default();
    for addr in ips {
        let cidr = format!("{addr}/{}", seg.prefix);
        if present.contains(&format!("inet {addr}/")) {
            return Err(format!(
                "segment {:?}: {addr} is already on {interface}, as someone else's address -- \
                 pick a free one so the units do not collide with a real host",
                seg.name
            ));
        }
        let mut args = s(&["addr", "add", &cidr, "dev", interface]);
        if labelled {
            args.extend(s(&["label", &label]));
        }
        ip(&args).map_err(privileged_hint)?;
        guard.push(s(&["addr", "del", &cidr, "dev", interface]));
    }
    info!(
        "network",
        "segment {}: {} address(es) attached to {interface}",
        seg.name,
        ips.len()
    );
    Ok(())
}

/// Bind every unit address once, so a missing one fails the start.
fn check_bindable(bp: &Blueprint) -> Result<(), String> {
    for seg in &bp.segments {
        for addr in addresses(bp, seg) {
            if let Err(e) = TcpListener::bind(SocketAddr::new(IpAddr::V4(addr), 0)) {
                let hint = match seg.mode {
                    SegmentMode::Existing => {
                        "mode=\"existing\" needs addresses this machine already has; \
                         `ipconfig` (Windows) or `ip addr` (Linux) lists them"
                    }
                    SegmentMode::Loopback => "this system does not answer that loopback address",
                    _ => "the interface was set up but the address is not usable",
                };
                return Err(format!(
                    "segment {:?}: cannot use {addr}: {e}. {hint}",
                    seg.name
                ));
            }
        }
    }
    Ok(())
}
