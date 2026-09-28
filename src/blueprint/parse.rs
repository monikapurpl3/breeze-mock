//! Blueprint XML to [`Blueprint`].
//!
//! Each function reads one element: its attributes, then its children through
//! a [`Seq`] in the order the schema fixes, then [`El::done`] to refuse
//! anything left over. Cross-references are checked last, once everything is
//! known, in [`check`].
//!
//! Merging rule for models: a unit's `<epithets>` *add* to its model's (traits
//! appended, labels joined); every other element the unit gives *replaces*
//! the model's element whole. Nothing is merged attribute by attribute, so
//! what a unit gets is always visible from two elements, never a blend.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use breeze_proto::ac::types::{FanSpeed, Mode, SwingMode};
use sha2::{Digest, Sha256};

use super::model::*;
use super::profiles;
use super::xml::{self, El, Error, Result, Seq};

const ROOT_ORDER: &[&str] = &[
    "include",
    "environment",
    "building",
    "network",
    "model",
    "unit",
    "server",
    "console",
];
const UNIT_ORDER: &[&str] = &[
    "epithets",
    "identity",
    "v2",
    "v3",
    "hardware",
    "capabilities",
    "state",
    "behaviour",
];
const MODEL_ORDER: &[&str] = &[
    "identity",
    "hardware",
    "capabilities",
    "state",
    "epithets",
    "behaviour",
];
const SERVER_ORDER: &[&str] = &[
    "build", "host", "listen", "domain", "pairing", "manage", "fault",
];

pub const MODES: &[&str] = &["auto", "cool", "dry", "heat", "fan-only"];
pub const TRAITS: &[&str] = &[
    "realistic",
    "flaky",
    "slow",
    "hangs-up",
    "warmup-lock",
    "eco-cool-only",
    "no-flaps-when-off",
    "no-turbo-in",
    "sleepy",
    "forgetful",
    "estranged",
    "deaf-after-handshake",
    "noisy-sensor",
    "laggy",
];

pub fn mode(s: &str) -> std::result::Result<Mode, String> {
    Ok(match xml::one_of(MODES)(s)?.as_str() {
        "auto" => Mode::Auto,
        "cool" => Mode::Cool,
        "dry" => Mode::Dry,
        "heat" => Mode::Heat,
        _ => Mode::FanOnly,
    })
}

pub fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Auto => "auto",
        Mode::Cool => "cool",
        Mode::Dry => "dry",
        Mode::Heat => "heat",
        Mode::FanOnly => "fan-only",
    }
}

fn swing(s: &str) -> std::result::Result<SwingMode, String> {
    Ok(
        match xml::one_of(&["off", "vertical", "horizontal", "both"])(s)?.as_str() {
            "off" => SwingMode::Off,
            "vertical" => SwingMode::Vertical,
            "horizontal" => SwingMode::Horizontal,
            _ => SwingMode::Both,
        },
    )
}

fn fan_setting(s: &str) -> std::result::Result<u8, String> {
    Ok(
        match xml::one_of(&["silent", "low", "medium", "high", "full", "auto"])(s)?.as_str() {
            "silent" => FanSpeed::SILENT.0,
            "low" => FanSpeed::LOW.0,
            "medium" => FanSpeed::MEDIUM.0,
            "high" => FanSpeed::HIGH.0,
            "full" => FanSpeed::FULL.0,
            _ => FanSpeed::AUTO.0,
        },
    )
}

fn fan_step(s: &str) -> std::result::Result<FanStep, String> {
    Ok(
        match xml::one_of(&["silent", "low", "medium", "high", "auto"])(s)?.as_str() {
            "silent" => FanStep::Silent,
            "low" => FanStep::Low,
            "medium" => FanStep::Medium,
            "high" => FanStep::High,
            _ => FanStep::Auto,
        },
    )
}

/// A setpoint: 16 to 30, whole or half degrees.
fn setpoint(s: &str) -> std::result::Result<f64, String> {
    let v = xml::decimal(s)?;
    if !(16.0..=30.0).contains(&v) || (v * 2.0).fract() != 0.0 {
        return Err("a target is 16 to 30 in whole or half degrees".into());
    }
    Ok(v)
}

fn ipv4(s: &str) -> std::result::Result<Ipv4Addr, String> {
    let ip: Ipv4Addr = s
        .parse()
        .map_err(|_| "expected an IPv4 address like 10.77.1.21")?;
    // Reject the forms Rust's parser accepts but nobody writes on purpose.
    if ip.to_string() != s {
        return Err("write the address in plain dotted form".into());
    }
    Ok(ip)
}

fn ip(s: &str) -> std::result::Result<IpAddr, String> {
    let ip: IpAddr = s
        .parse()
        .map_err(|_| "expected an IP address like 127.0.0.1 or ::1")?;
    if ip.to_string() != s {
        return Err("write the address in its canonical form".into());
    }
    Ok(ip)
}

fn cidr(s: &str) -> std::result::Result<(Ipv4Addr, u8), String> {
    let (a, p) = s
        .split_once('/')
        .ok_or("expected a CIDR like 10.77.1.0/24")?;
    let addr = ipv4(a)?;
    let prefix: u8 = xml::unsigned(p)?;
    if !(8..=30).contains(&prefix) {
        return Err("the prefix must be between /8 and /30".into());
    }
    let mask = u32::MAX << (32 - prefix);
    if u32::from(addr) & !mask != 0 {
        return Err(format!(
            "{addr} is not the network address of a /{prefix} (that would be {})",
            Ipv4Addr::from(u32::from(addr) & mask)
        ));
    }
    Ok((addr, prefix))
}

/// A DNS hostname: lowercase labels of letters, digits and inner hyphens.
fn hostname(s: &str) -> std::result::Result<String, String> {
    let ok = s.len() <= 253
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        });
    if !ok {
        return Err("expected a lowercase hostname like breeze.test".into());
    }
    Ok(s.to_string())
}

fn path_in(base: &Path) -> impl Fn(&str) -> std::result::Result<PathBuf, String> + '_ {
    move |s| {
        let p = Path::new(s);
        Ok(if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        })
    }
}

/// A value that is either `auto` or parsed by `f`.
fn auto_or<T>(
    f: impl Fn(&str) -> std::result::Result<T, String>,
) -> impl Fn(&str) -> std::result::Result<Option<T>, String> {
    move |s| {
        if s == "auto" {
            Ok(None)
        } else {
            f(s).map(Some)
        }
    }
}

// ----------------------------------------------------------------- loader

/// A model: every element optional, resolved into units that name it.
#[derive(Debug, Clone, Default)]
struct ModelDef {
    identity: Option<IdentityDef>,
    hardware: Option<Hardware>,
    capabilities: Option<CapabilitySet>,
    state: Option<InitialState>,
    epithets: Option<EpithetsDef>,
    behaviour: Option<BehaviourPatch>,
}

#[derive(Debug, Clone, Default)]
struct IdentityDef {
    model: Option<String>,
    serial: Option<String>,
    ssid: Option<String>,
    firmware: Option<String>,
    device_type: Option<u8>,
}

#[derive(Debug, Clone, Default)]
struct EpithetsDef {
    name: Option<String>,
    nickname: Option<String>,
    labels: Vec<String>,
    traits: Vec<Trait>,
}

#[derive(Debug, Clone)]
struct Trait {
    name: String,
    apply: fn(&mut Behaviour, &TraitArgs),
    args: TraitArgs,
}

#[derive(Debug, Clone, Default)]
struct TraitArgs {
    rate: Option<f64>,
    delay: Option<Duration>,
    jitter: Option<Duration>,
    after: Option<Duration>,
    window: Option<(f64, f64)>,
    modes: Vec<Mode>,
    amplitude: Option<f64>,
}

/// `<behaviour>`: explicit settings applied after the traits.
#[derive(Debug, Clone, Default)]
struct BehaviourPatch {
    reply_delay: Option<Duration>,
    jitter: Option<Duration>,
    drop_rate: Option<f64>,
    idle_hangup: Option<Option<Duration>>,
    heat_warmup: Option<Option<Duration>>,
    eco_cool_only: Option<bool>,
    no_flaps_when_off: Option<bool>,
    reject_handshake: Option<bool>,
    post_handshake_deafness: Option<Duration>,
    session_lifetime: Option<Option<Duration>>,
    sensor_noise: Option<f64>,
    report_lag: Option<Duration>,
}

pub fn load(path: &Path) -> Result<Blueprint> {
    let file = path.to_path_buf();
    let text = read(&file, None)?;
    let doc = xml::parse_document(&file, &text)?;
    let root = El::new(doc.root_element(), &file)?;
    if root.name() != "blueprint" {
        return Err(root.err("the root element of a blueprint must be <blueprint>"));
    }
    blueprint(&root)
}

fn read(file: &Path, from: Option<&El>) -> Result<String> {
    std::fs::read_to_string(file).map_err(|e| {
        let message = format!("cannot read {}: {e}", file.display());
        match from {
            Some(el) => el.err(message),
            None => Error {
                file: file.to_path_buf(),
                line: 0,
                col: 0,
                message,
            },
        }
    })
}

fn blueprint(el: &El) -> Result<Blueprint> {
    el.need("version", xml::one_of(&["1"]))?;
    let name = el.need("name", xml::ident)?;
    let seed = el.get("seed", xml::unsigned::<u64>)?.unwrap_or(0);
    el.done()?;
    let base = el.file().parent().unwrap_or(Path::new(".")).to_path_buf();

    let mut seq = Seq::new(el, ROOT_ORDER)?;
    let mut models: BTreeMap<String, (ModelDef, Error)> = BTreeMap::new();
    let mut seen_files = BTreeSet::new();
    seen_files.insert(
        el.file()
            .canonicalize()
            .unwrap_or_else(|_| el.file().to_path_buf()),
    );
    for inc in seq.many("include")? {
        let href = inc.need("href", path_in(&base))?;
        inc.done()?;
        inc.empty()?;
        library(&inc, &href, &mut models, &mut seen_files)?;
    }
    let environment = match seq.maybe("environment")? {
        Some(e) => environment(&e)?,
        None => Environment {
            time_scale: 1.0,
            tick: Duration::from_secs(1),
            weather: Weather::Fixed(20.0),
        },
    };
    let building = match seq.maybe("building")? {
        Some(b) => building(&b)?,
        None => Building::default(),
    };
    let segments = match seq.maybe("network")? {
        Some(n) => network(&n)?,
        None => Vec::new(),
    };
    for m in seq.many("model")? {
        let (name, def) = model(&m)?;
        if let Some((_, first)) = models.get(&name) {
            return Err(m.err(format!("model {name:?} is already defined at {first}")));
        }
        models.insert(name, (def, m.err("")));
    }
    let mut units = Vec::new();
    for u in seq.many("unit")? {
        units.push(unit(&u, &models, seed)?);
    }
    let mut servers = Vec::new();
    for s in seq.many("server")? {
        servers.push(server(&s, &base, seed)?);
    }
    let console = match seq.maybe("console")? {
        Some(c) => {
            let address = c.need("address", ip)?;
            let port = c.need("port", xml::unsigned::<u16>)?;
            c.done()?;
            c.empty()?;
            Some(Console { address, port })
        }
        None => None,
    };
    seq.end()?;

    let bp = Blueprint {
        name,
        source: el.file().to_path_buf(),
        seed,
        environment,
        building,
        segments,
        units,
        servers,
        console,
    };
    check(el, &bp)?;
    Ok(bp)
}

/// `<include href>`: a `<library>` of models, from another file.
fn library(
    inc: &El,
    href: &Path,
    models: &mut BTreeMap<String, (ModelDef, Error)>,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let canon = href
        .canonicalize()
        .map_err(|e| inc.err(format!("cannot read {}: {e}", href.display())))?;
    if !seen.insert(canon) {
        return Err(inc.err(format!("{} is included more than once", href.display())));
    }
    let text = read(href, Some(inc))?;
    let doc = xml::parse_document(href, &text)?;
    let root = El::new(doc.root_element(), href)?;
    if root.name() != "library" {
        return Err(root.err("an included file must be a <library> of models"));
    }
    root.need("version", xml::one_of(&["1"]))?;
    root.done()?;
    let mut seq = Seq::new(&root, &["model"])?;
    for m in seq.take("model", 1, usize::MAX)? {
        let (name, def) = model(&m)?;
        if let Some((_, first)) = models.get(&name) {
            return Err(m.err(format!("model {name:?} is already defined at {first}")));
        }
        models.insert(name, (def, m.err("")));
    }
    seq.end()
}

// ------------------------------------------------------------------ world

fn environment(el: &El) -> Result<Environment> {
    let time_scale = el.get("time-scale", xml::positive)?.unwrap_or(1.0);
    if time_scale > 3600.0 {
        return Err(el.err("time-scale is at most 3600 (an hour a second)"));
    }
    let tick = el
        .get("tick", xml::duration)?
        .unwrap_or(Duration::from_secs(1));
    if tick < Duration::from_millis(50) || tick > Duration::from_secs(60) {
        return Err(el.err("tick must be between 50ms and 60s"));
    }
    el.done()?;
    // Exactly one way of saying what the weather does.
    let mut seq = Seq::new(el, &["constant", "daily", "hourly"])?;
    let constant = seq.maybe("constant")?;
    let daily = seq.maybe("daily")?;
    let hourly = seq.maybe("hourly")?;
    seq.end()?;
    let weather = match (constant, daily, hourly) {
        (Some(c), None, None) => {
            let t = c.need("outdoor", xml::decimal)?;
            c.done()?;
            c.empty()?;
            Weather::Fixed(t)
        }
        (None, Some(d), None) => {
            let w = Weather::Daily {
                mean: d.need("mean", xml::decimal)?,
                swing: d.need("swing", |s| {
                    xml::unsigned_decimal(s)
                        .ok_or_else(|| "expected a non-negative decimal".to_string())
                })?,
                warmest_hour: d.need("warmest", xml::clock)?,
            };
            d.done()?;
            d.empty()?;
            w
        }
        (None, None, Some(h)) => {
            h.done()?;
            let mut pts = Seq::new(&h, &["at"])?;
            let mut out: Vec<(f64, f64)> = Vec::new();
            for p in pts.take("at", 2, usize::MAX)? {
                let hour = p.need("time", xml::clock)?;
                let t = p.need("outdoor", xml::decimal)?;
                p.done()?;
                p.empty()?;
                if out.last().is_some_and(|(prev, _)| *prev >= hour) {
                    return Err(p.err("<at> points must be in increasing time order"));
                }
                out.push((hour, t));
            }
            pts.end()?;
            Weather::Points(out)
        }
        (None, None, None) => {
            return Err(el.err("needs one of <constant>, <daily> or <hourly>"));
        }
        _ => return Err(el.err("give only one of <constant>, <daily> or <hourly>")),
    };
    Ok(Environment {
        time_scale,
        tick,
        weather,
    })
}

fn building(el: &El) -> Result<Building> {
    let name = el.need("name", xml::text)?;
    el.done()?;
    let mut seq = Seq::new(el, &["floor"])?;
    let mut floors = Vec::new();
    for (i, f) in seq.take("floor", 1, usize::MAX)?.iter().enumerate() {
        let fname = f.need("name", xml::text)?;
        let level = f
            .get("level", |s| {
                xml::integer(s)
                    .and_then(|v| i32::try_from(v).map_err(|_| "out of range".to_string()))
            })?
            .unwrap_or(i as i32);
        f.done()?;
        let mut rooms_seq = Seq::new(f, &["room"])?;
        let mut rooms = Vec::new();
        for r in rooms_seq.take("room", 1, usize::MAX)? {
            rooms.push(room(&r)?);
        }
        rooms_seq.end()?;
        floors.push(Floor {
            name: fname,
            level,
            rooms,
        });
    }
    seq.end()?;
    Ok(Building { name, floors })
}

fn room(el: &El) -> Result<Room> {
    let id = el.need("id", xml::ident)?;
    let name = el.need("name", xml::text)?;
    let area = el.need("area", xml::positive)?;
    let height = el.get("height", xml::positive)?.unwrap_or(2.5);
    // A word for the usual cases, or the figure itself: insulation="85W/K".
    let loss = el.get("insulation", |s| {
        // Heat loss per square metre of floor, per kelvin: rough whole-room
        // figures, enough for a room to behave like a room.
        Ok(match s {
            "poor" => 6.0 * area,
            "average" => 3.5 * area,
            "good" => 2.0 * area,
            "passive" => 0.8 * area,
            _ => {
                let n = s
                    .strip_suffix("W/K")
                    .ok_or("expected poor, average, good, passive or a figure like 85W/K")?;
                xml::positive(n)?
            }
        })
    })?;
    let start = el.get("start", xml::decimal)?.unwrap_or(21.0);
    let gains = el.get("gains", xml::watts)?.unwrap_or(0.0);
    let sun = el.get("sun", xml::watts)?.unwrap_or(0.0);
    el.done()?;
    el.empty()?;
    Ok(Room {
        origin: el.origin(),
        id,
        name,
        area_m2: area,
        height_m: height,
        loss_w_per_k: loss.unwrap_or(3.5 * area),
        // Air alone would change temperature in seconds; furniture, walls
        // and floor slow a real room down roughly eightfold.
        capacity_j_per_k: area * height * 1.2 * 1005.0 * 8.0,
        internal_gain_w: gains,
        sun_gain_w: sun,
        start_c: start,
    })
}

// --------------------------------------------------------------- network

fn network(el: &El) -> Result<Vec<Segment>> {
    el.done()?;
    let mut seq = Seq::new(el, &["segment"])?;
    let mut out: Vec<Segment> = Vec::new();
    for s in seq.take("segment", 1, usize::MAX)? {
        let name = s.need("name", xml::ident)?;
        let (network, prefix) = s.need("cidr", cidr)?;
        let mode_word = s.need(
            "mode",
            xml::one_of(&["loopback", "dummy", "attach", "existing"]),
        )?;
        let interface = s.get("interface", |v| {
            let n = xml::ident(v)?;
            if n.len() > 15 {
                return Err("Linux interface names are at most 15 characters".into());
            }
            Ok(n)
        })?;
        let discovery = s.get("discovery", xml::boolean)?.unwrap_or(true);
        s.done()?;
        s.empty()?;
        let seg = Segment {
            origin: s.origin(),
            name: name.clone(),
            network,
            prefix,
            discovery,
            mode: match mode_word.as_str() {
                "loopback" | "existing" if interface.is_some() => {
                    return Err(s.err(format!("mode=\"{mode_word}\" takes no interface=")));
                }
                "loopback" => SegmentMode::Loopback,
                "existing" => SegmentMode::Existing,
                "dummy" => SegmentMode::Dummy {
                    interface: interface.unwrap_or_else(|| {
                        let mut n = format!("bm-{name}");
                        n.truncate(15);
                        n.trim_end_matches('-').to_string()
                    }),
                },
                _ => SegmentMode::Attach {
                    interface: interface.ok_or_else(|| {
                        s.err("mode=\"attach\" needs the interface= to add addresses to")
                    })?,
                },
            },
        };
        let is_loop = network.octets()[0] == 127;
        match &seg.mode {
            SegmentMode::Loopback if !is_loop => {
                return Err(s.err("a loopback segment must be inside 127.0.0.0/8"));
            }
            SegmentMode::Dummy { .. } | SegmentMode::Attach { .. } if !is_private(network) => {
                return Err(s.err(
                    "dummy and attach segments must be private (10/8, 172.16/12 or 192.168/16), \
                     which is also the only range Breeze Core will scan",
                ));
            }
            _ => {}
        }
        for other in &out {
            if other.name == seg.name {
                return Err(s.err(format!("segment {:?} is defined twice", seg.name)));
            }
            if other.contains(seg.network) || seg.contains(other.network) {
                return Err(s.err(format!(
                    "{} overlaps segment {:?} ({})",
                    seg.cidr(),
                    other.name,
                    other.cidr()
                )));
            }
        }
        out.push(seg);
    }
    seq.end()?;
    Ok(out)
}

pub fn is_private(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
}

// ------------------------------------------------------------------ units

fn model(el: &El) -> Result<(String, ModelDef)> {
    let name = el.need("name", xml::ident)?;
    el.done()?;
    let mut seq = Seq::new(el, MODEL_ORDER)?;
    let def = ModelDef {
        identity: seq
            .maybe("identity")?
            .map(|e| identity(&e, true))
            .transpose()?,
        hardware: seq.maybe("hardware")?.map(|e| hardware(&e)).transpose()?,
        capabilities: seq
            .maybe("capabilities")?
            .map(|e| capabilities(&e))
            .transpose()?,
        state: seq.maybe("state")?.map(|e| state(&e)).transpose()?,
        epithets: seq
            .maybe("epithets")?
            .map(|e| epithets(&e, true))
            .transpose()?,
        behaviour: seq.maybe("behaviour")?.map(|e| behaviour(&e)).transpose()?,
    };
    seq.end()?;
    Ok((name, def))
}

fn unit(el: &El, models: &BTreeMap<String, (ModelDef, Error)>, seed: u64) -> Result<Unit> {
    let key = el.need("key", xml::ident)?;
    let id = el
        .get(
            "id",
            auto_or(|s| {
                let v: u64 = xml::unsigned(s)?;
                if v == 0 || v >= 1 << 48 {
                    return Err("a Midea device id is 1 to 2^48-1".into());
                }
                Ok(v)
            }),
        )?
        .flatten();
    let model_name = el.get("model", xml::ident)?;
    let segment = el.need("segment", xml::ident)?;
    let ip = el.need("ip", ipv4)?;
    let room = el.get("room", xml::ident)?;
    el.done()?;

    let base = match &model_name {
        Some(m) => models.get(m).map(|(d, _)| d.clone()).ok_or_else(|| {
            el.err(format!(
                "no model named {m:?}{}",
                xml::suggest(m, &models.keys().map(String::as_str).collect::<Vec<_>>())
            ))
        })?,
        None => ModelDef::default(),
    };

    let mut seq = Seq::new(el, UNIT_ORDER)?;
    let own_epithets = epithets(&seq.one("epithets")?, false)?;
    let own_identity = seq
        .maybe("identity")?
        .map(|e| identity(&e, false))
        .transpose()?;
    let v2 = seq.maybe("v2")?;
    let v3 = seq.maybe("v3")?;
    let protocol_el = match (v2, v3) {
        (Some(p), None) | (None, Some(p)) => p,
        (None, None) => {
            return Err(el.err("needs its protocol: <v3 token=\"auto\" key=\"auto\"/> or <v2/>"))
        }
        (Some(_), Some(p)) => {
            return Err(p.err("a unit speaks one protocol: <v2> or <v3>, not both"))
        }
    };
    let own_hardware = seq.maybe("hardware")?.map(|e| hardware(&e)).transpose()?;
    let own_caps = seq
        .maybe("capabilities")?
        .map(|e| capabilities(&e))
        .transpose()?;
    let own_state = seq.maybe("state")?.map(|e| state(&e)).transpose()?;
    let own_behaviour = seq.maybe("behaviour")?.map(|e| behaviour(&e)).transpose()?;
    seq.end()?;

    let id = id.unwrap_or_else(|| derive_id(seed, &key));
    let protocol = protocol(&protocol_el, seed, &key)?;

    // Epithets add; everything else replaces.
    let mut ep = base.epithets.clone().unwrap_or_default();
    ep.name = own_epithets.name.clone();
    ep.nickname = own_epithets.nickname.clone();
    for l in own_epithets.labels {
        if !ep.labels.contains(&l) {
            ep.labels.push(l);
        }
    }
    ep.traits.extend(own_epithets.traits);

    let idef = own_identity.or(base.identity).unwrap_or_default();
    let device_type = idef.device_type.unwrap_or(0xAC);
    let serial = idef
        .serial
        .unwrap_or_else(|| derive_serial(seed, &key, device_type));
    let ssid = idef
        .ssid
        .unwrap_or_else(|| format!("net_{device_type:02x}_{}", &serial[24..28]));
    let identity = Identity {
        name: ep.name.clone().expect("a unit's epithets require name="),
        nickname: ep.nickname.clone(),
        model: idef.model.unwrap_or_else(|| {
            model_name
                .clone()
                .map(|m| m.to_uppercase())
                .unwrap_or_else(|| "MOCK-AC".into())
        }),
        serial,
        ssid,
        device_type,
        firmware: idef.firmware.unwrap_or_else(|| "breeze-mock".into()),
    };

    let mut behaviour = Behaviour::default();
    for t in &ep.traits {
        (t.apply)(&mut behaviour, &t.args);
    }
    if let Some(p) = own_behaviour.or(base.behaviour) {
        p.apply(&mut behaviour);
    }

    let capabilities = own_caps
        .or(base.capabilities)
        .unwrap_or_else(default_capabilities);
    let state = own_state.or(base.state).unwrap_or_else(default_state);
    if !capabilities.modes.contains(&state.mode) {
        return Err(el.err(format!(
            "starts in mode {:?}, which its capabilities do not list",
            mode_name(state.mode)
        )));
    }

    Ok(Unit {
        origin: el.origin(),
        key,
        id,
        identity,
        protocol,
        segment,
        ip,
        room,
        hardware: own_hardware
            .or(base.hardware)
            .unwrap_or_else(default_hardware),
        capabilities,
        state,
        epithets: ep.traits.iter().map(|t| t.name.clone()).collect(),
        behaviour,
        model: model_name,
    })
}

fn identity(el: &El, in_model: bool) -> Result<IdentityDef> {
    let model = el.get("model", xml::text)?;
    let serial = el.get("serial", |s| {
        if s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            Ok(s.to_string())
        } else {
            Err("a serial is exactly 32 characters of A-Z and 0-9".into())
        }
    })?;
    let ssid = el.get("ssid", |s| {
        let parts: Vec<&str> = s.split('_').collect();
        let lower_hex =
            |p: &str| p.len() == 2 && p.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        let suffix =
            |p: &str| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric());
        if parts.len() == 3 && parts[0] == "net" && lower_hex(parts[1]) && suffix(parts[2]) {
            Ok(s.to_string())
        } else {
            Err("an SSID looks like net_ac_23E4: net, the device type in hex, a suffix".into())
        }
    })?;
    let firmware = el.get("firmware", xml::text)?;
    let device_type = el.get("type", xml::hex8)?;
    el.done()?;
    el.empty()?;
    if in_model && (serial.is_some() || ssid.is_some()) {
        return Err(el.err("serial= and ssid= belong to one unit, not to a model"));
    }
    Ok(IdentityDef {
        model,
        serial,
        ssid,
        firmware,
        device_type,
    })
}

/// `<v3 token key port>` or `<v2 port>`: the element *is* the protocol, so a
/// V2 unit cannot be given credentials it would never use.
fn protocol(el: &El, seed: u64, key: &str) -> Result<Protocol> {
    let port = el.get("port", xml::unsigned::<u16>)?.unwrap_or(6444);
    if port == 0 {
        return Err(el.err("port 0 is not a port"));
    }
    if el.name() == "v2" {
        el.done()?;
        el.empty()?;
        return Ok(Protocol {
            version: ProtocolVersion::V2,
            port,
            token: Vec::new(),
            key: [0; 32],
        });
    }
    let token = el.need("token", auto_or(xml::hex_exact(64)))?;
    let secret = el.need("key", auto_or(xml::hex_exact(32)))?;
    el.done()?;
    el.empty()?;
    let token = token.unwrap_or_else(|| derive_bytes(seed, key, "token", 64));
    let secret = secret.unwrap_or_else(|| derive_bytes(seed, key, "key", 32));
    let mut k = [0u8; 32];
    k.copy_from_slice(&secret);
    Ok(Protocol {
        version: ProtocolVersion::V3,
        port,
        token,
        key: k,
    })
}

fn hardware(el: &El) -> Result<Hardware> {
    let h = Hardware {
        cooling_w: el.need("cooling", xml::watts)?,
        heating_w: el.need("heating", xml::watts)?,
        indoor_sensor: el.get("indoor-sensor", xml::boolean)?.unwrap_or(true),
        outdoor_sensor: el.get("outdoor-sensor", xml::boolean)?.unwrap_or(true),
        indoor_offset: el.get("indoor-offset", xml::decimal)?.unwrap_or(0.0),
        outdoor_offset: el.get("outdoor-offset", xml::decimal)?.unwrap_or(0.0),
    };
    el.done()?;
    el.empty()?;
    Ok(h)
}

fn capabilities(el: &El) -> Result<CapabilitySet> {
    let modes = el.need("modes", xml::list(mode))?;
    let swing = el.need("swing", |s| {
        if s == "none" {
            return Ok(Vec::new());
        }
        xml::list(xml::one_of(&["vertical", "horizontal"]))(s)
    })?;
    let fan = el.need("fan", xml::list(fan_step))?;
    let turbo = el
        .get("turbo", |s| {
            if s == "none" {
                return Ok(Vec::new());
            }
            xml::list(xml::one_of(&["cool", "heat"]))(s)
        })?
        .unwrap_or_default();
    let flag = |name: &str, default: bool| -> Result<bool> {
        Ok(el.get(name, xml::boolean)?.unwrap_or(default))
    };
    let caps = CapabilitySet {
        modes,
        swing_vertical: swing.iter().any(|s| s == "vertical"),
        swing_horizontal: swing.iter().any(|s| s == "horizontal"),
        fan,
        fan_custom: flag("fan-custom", false)?,
        eco: flag("eco", false)?,
        turbo_cool: turbo.iter().any(|t| t == "cool"),
        turbo_heat: turbo.iter().any(|t| t == "heat"),
        freeze_protection: flag("freeze-protection", false)?,
        display_control: flag("display", false)?,
        humidity: flag("humidity", false)?,
        buzzer: flag("buzzer", false)?,
        fahrenheit: flag("fahrenheit", true)?,
        cool_range: el.get("cool", xml::range)?.unwrap_or((17.0, 30.0)),
        auto_range: el.get("auto", xml::range)?.unwrap_or((17.0, 30.0)),
        heat_range: el.get("heat", xml::range)?.unwrap_or((17.0, 30.0)),
        half_degrees: flag("half-degrees", true)?,
        two_pages: el.get("pages", xml::one_of(&["1", "2"]))?.as_deref() == Some("2"),
        extra_records: Vec::new(),
    };
    el.done()?;
    let mut caps = caps;
    for r in ["cool", "auto", "heat"] {
        let (lo, hi) = match r {
            "cool" => caps.cool_range,
            "auto" => caps.auto_range,
            _ => caps.heat_range,
        };
        if lo < 16.0 || hi > 30.0 || (lo * 2.0).fract() != 0.0 || (hi * 2.0).fract() != 0.0 {
            return Err(el.err(format!(
                "{r}= must lie within 16-30, in whole or half degrees"
            )));
        }
    }
    let mut seq = Seq::new(el, &["record"])?;
    for r in seq.many("record")? {
        let id = r.need("id", xml::hex16)?;
        let value = r.need("value", xml::hex)?;
        r.done()?;
        r.empty()?;
        if value.is_empty() || value.len() > 32 {
            return Err(r.err("a record's value is 1 to 32 bytes"));
        }
        if caps.extra_records.iter().any(|(i, _)| *i == id) {
            return Err(r.err(format!("record 0x{id:04x} is given twice")));
        }
        caps.extra_records.push((id, value));
    }
    seq.end()?;
    Ok(caps)
}

fn state(el: &El) -> Result<InitialState> {
    let s = InitialState {
        power: el.need("power", xml::one_of(&["on", "off"]))? == "on",
        mode: el.need("mode", mode)?,
        target: el.need("target", setpoint)?,
        fan: el.get("fan", fan_setting)?.unwrap_or(FanSpeed::AUTO.0),
        swing: el.get("swing", swing)?.unwrap_or(SwingMode::Off),
        eco: el.get("eco", xml::boolean)?.unwrap_or(false),
        turbo: el.get("turbo", xml::boolean)?.unwrap_or(false),
        sleep: el.get("sleep", xml::boolean)?.unwrap_or(false),
        fahrenheit: el.get("fahrenheit", xml::boolean)?.unwrap_or(false),
        display: el.get("display", xml::boolean)?.unwrap_or(true),
        humidity: el
            .get("humidity", |v| {
                let h: u8 = xml::unsigned(v)?;
                if h > 100 {
                    Err("humidity is 0 to 100".into())
                } else {
                    Ok(h)
                }
            })?
            .unwrap_or(40),
        error_code: el.get("error", xml::unsigned::<u8>)?.unwrap_or(0),
    };
    el.done()?;
    el.empty()?;
    Ok(s)
}

fn epithets(el: &El, in_model: bool) -> Result<EpithetsDef> {
    let name = el.get("name", xml::text)?;
    let nickname = el.get("nickname", xml::ident)?;
    let labels = el.get("labels", xml::list(xml::ident))?.unwrap_or_default();
    el.done()?;
    match (in_model, &name) {
        (true, Some(_)) => {
            return Err(el.err("a model's epithets carry no name=; names belong to units"))
        }
        (true, None) if nickname.is_some() => {
            return Err(el.err("a model's epithets carry no nickname="))
        }
        (false, None) => {
            return Err(el.err("a unit's <epithets> needs name=, what Breeze Core will call it"))
        }
        _ => {}
    }
    // One element per trait, in this order, each at most once -- except
    // <sleepy>, which may give several windows.
    let mut seq = Seq::new(el, TRAITS)?;
    let mut traits = Vec::new();
    for t in TRAITS {
        let max = if *t == "sleepy" { usize::MAX } else { 1 };
        for e in seq.take(t, 0, max)? {
            traits.push(epithet(&e)?);
        }
    }
    seq.end()?;
    Ok(EpithetsDef {
        name,
        nickname,
        labels,
        traits,
    })
}

/// One trait. Each takes exactly the attributes listed for it, so
/// `<flaky delay="2s"/>` is an error rather than a flaky unit that is not slow.
fn epithet(el: &El) -> Result<Trait> {
    let name = el.name().to_string();
    let mut a = TraitArgs::default();
    let apply: fn(&mut Behaviour, &TraitArgs) = match name.as_str() {
        "realistic" => |b, _| {
            b.reply_delay = Duration::from_millis(720);
            b.drop_rate = 1.0 / 30.0;
            b.idle_hangup = Some(Duration::from_secs(30));
            b.heat_warmup = Some(Duration::from_secs(180));
            b.eco_cool_only = true;
            b.no_flaps_when_off = true;
            b.post_handshake_deafness = Duration::from_secs(1);
        },
        "flaky" => {
            a.rate = Some(el.get("rate", xml::rate)?.unwrap_or(1.0 / 30.0));
            |b, a| b.drop_rate = a.rate.unwrap_or(0.0)
        }
        "slow" => {
            a.delay = Some(
                el.get("delay", xml::duration)?
                    .unwrap_or(Duration::from_millis(720)),
            );
            a.jitter = el.get("jitter", xml::duration)?;
            |b, a| {
                b.reply_delay = a.delay.unwrap_or_default();
                b.jitter = a.jitter.unwrap_or_default();
            }
        }
        "hangs-up" => {
            a.after = Some(
                el.get("after", xml::duration)?
                    .unwrap_or(Duration::from_secs(30)),
            );
            |b, a| b.idle_hangup = a.after
        }
        "warmup-lock" => {
            a.after = Some(
                el.get("for", xml::duration)?
                    .unwrap_or(Duration::from_secs(180)),
            );
            |b, a| b.heat_warmup = a.after
        }
        "eco-cool-only" => |b, _| b.eco_cool_only = true,
        "no-flaps-when-off" => |b, _| b.no_flaps_when_off = true,
        "no-turbo-in" => {
            a.modes = el.need("modes", xml::list(mode))?;
            |b, a| b.no_turbo_in.extend(a.modes.iter().copied())
        }
        "sleepy" => {
            let from = el.need("from", xml::clock)?;
            let to = el.need("to", xml::clock)?;
            if from == to {
                return Err(el.err("from= and to= are the same time"));
            }
            a.window = Some((from, to));
            |b, a| b.offline.extend(a.window)
        }
        "forgetful" => {
            a.after = Some(el.need("every", xml::duration)?);
            |b, a| b.session_lifetime = a.after
        }
        "estranged" => |b, _| b.reject_handshake = true,
        "deaf-after-handshake" => {
            a.after = Some(
                el.get("for", xml::duration)?
                    .unwrap_or(Duration::from_secs(1)),
            );
            |b, a| b.post_handshake_deafness = a.after.unwrap_or_default()
        }
        "noisy-sensor" => {
            a.amplitude = Some(el.get("amplitude", xml::positive)?.unwrap_or(0.3));
            |b, a| b.sensor_noise = a.amplitude.unwrap_or(0.0)
        }
        _ => {
            a.after = Some(el.need("lag", xml::duration)?);
            |b, a| b.report_lag = a.after.unwrap_or_default()
        }
    };
    el.done()?;
    el.empty()?;
    Ok(Trait {
        name,
        apply,
        args: a,
    })
}

fn behaviour(el: &El) -> Result<BehaviourPatch> {
    let none_or = |name: &str| -> Result<Option<Option<Duration>>> {
        el.get(name, |s| {
            if s == "never" {
                Ok(None)
            } else {
                xml::duration(s).map(Some)
            }
        })
    };
    let p = BehaviourPatch {
        reply_delay: el.get("reply-delay", xml::duration)?,
        jitter: el.get("jitter", xml::duration)?,
        drop_rate: el.get("drop-rate", xml::rate)?,
        idle_hangup: none_or("idle-hangup")?,
        heat_warmup: none_or("heat-warmup")?,
        eco_cool_only: el.get("eco-cool-only", xml::boolean)?,
        no_flaps_when_off: el.get("no-flaps-when-off", xml::boolean)?,
        reject_handshake: el.get("reject-handshake", xml::boolean)?,
        post_handshake_deafness: el.get("post-handshake-deafness", xml::duration)?,
        session_lifetime: none_or("session-lifetime")?,
        sensor_noise: el.get("sensor-noise", |s| {
            if s == "0" {
                Ok(0.0)
            } else {
                xml::positive(s)
            }
        })?,
        report_lag: el.get("report-lag", xml::duration)?,
    };
    el.done()?;
    el.empty()?;
    Ok(p)
}

impl BehaviourPatch {
    fn apply(&self, b: &mut Behaviour) {
        if let Some(v) = self.reply_delay {
            b.reply_delay = v;
        }
        if let Some(v) = self.jitter {
            b.jitter = v;
        }
        if let Some(v) = self.drop_rate {
            b.drop_rate = v;
        }
        if let Some(v) = self.idle_hangup {
            b.idle_hangup = v;
        }
        if let Some(v) = self.heat_warmup {
            b.heat_warmup = v;
        }
        if let Some(v) = self.eco_cool_only {
            b.eco_cool_only = v;
        }
        if let Some(v) = self.no_flaps_when_off {
            b.no_flaps_when_off = v;
        }
        if let Some(v) = self.reject_handshake {
            b.reject_handshake = v;
        }
        if let Some(v) = self.post_handshake_deafness {
            b.post_handshake_deafness = v;
        }
        if let Some(v) = self.session_lifetime {
            b.session_lifetime = v;
        }
        if let Some(v) = self.sensor_noise {
            b.sensor_noise = v;
        }
        if let Some(v) = self.report_lag {
            b.report_lag = v;
        }
    }
}

fn default_hardware() -> Hardware {
    Hardware {
        cooling_w: 3500.0,
        heating_w: 3800.0,
        indoor_sensor: true,
        outdoor_sensor: true,
        indoor_offset: 0.0,
        outdoor_offset: 0.0,
    }
}

fn default_capabilities() -> CapabilitySet {
    CapabilitySet {
        modes: vec![Mode::Auto, Mode::Cool, Mode::Dry, Mode::Heat, Mode::FanOnly],
        swing_vertical: true,
        swing_horizontal: true,
        fan: vec![FanStep::Low, FanStep::Medium, FanStep::High, FanStep::Auto],
        fan_custom: false,
        eco: true,
        turbo_cool: true,
        turbo_heat: true,
        freeze_protection: false,
        display_control: true,
        humidity: false,
        buzzer: true,
        fahrenheit: true,
        cool_range: (17.0, 30.0),
        auto_range: (17.0, 30.0),
        heat_range: (17.0, 30.0),
        half_degrees: true,
        two_pages: false,
        extra_records: Vec::new(),
    }
}

fn default_state() -> InitialState {
    InitialState {
        power: false,
        mode: Mode::Cool,
        target: 24.0,
        fan: FanSpeed::AUTO.0,
        swing: SwingMode::Off,
        eco: false,
        turbo: false,
        sleep: false,
        fahrenheit: false,
        display: true,
        humidity: 40,
        error_code: 0,
    }
}

// ---------------------------------------------------------------- servers

fn server(el: &El, base: &Path, seed: u64) -> Result<Server> {
    let name = el.need("name", xml::ident)?;
    let api_key = el
        .get(
            "api-key",
            auto_or(|s| {
                if (16..=128).contains(&s.len())
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    Ok(s.to_string())
                } else {
                    Err("an API key is 16 to 128 characters of A-Z, a-z, 0-9, - and _".into())
                }
            }),
        )?
        .flatten();
    let latency = el.get("latency", xml::duration)?.unwrap_or_default();
    let clock_offset = el.get("clock-offset", xml::signed_seconds)?.unwrap_or(0);
    let state_dir = el.get("state-dir", path_in(base))?;
    el.done()?;

    let mut seq = Seq::new(el, SERVER_ORDER)?;
    let build = build(&seq.one("build")?)?;
    let host = match seq.maybe("host")? {
        Some(h) => host(&h)?,
        None => default_host(&name),
    };
    let mut listens = Vec::new();
    for l in seq.take("listen", 1, usize::MAX)? {
        let address = l.need("address", ip)?;
        let port = l.need("port", xml::unsigned::<u16>)?;
        let tls = l.get("tls", xml::boolean)?.unwrap_or(false);
        l.done()?;
        l.empty()?;
        if port == 0 {
            return Err(l.err("port 0 is not a port"));
        }
        if listens
            .iter()
            .any(|x: &Listen| x.address == address && x.port == port)
        {
            return Err(l.err(format!("{address} port {port} is listed twice")));
        }
        listens.push(Listen { address, port, tls });
    }
    let mut domains = Vec::new();
    for d in seq.many("domain")? {
        let dname = d.need("name", hostname)?;
        d.done()?;
        // No child: plain HTTP. <self-signed/>: a certificate from the run's
        // own CA. <files cert key/>: certificates of your own.
        let mut how = Seq::new(&d, &["self-signed", "files"])?;
        let own = how.maybe("self-signed")?;
        let files = how.maybe("files")?;
        how.end()?;
        let tls = match (own, files) {
            (None, None) => TlsSource::None,
            (Some(s), None) => {
                s.done()?;
                s.empty()?;
                TlsSource::SelfSigned
            }
            (None, Some(f)) => {
                let cert = f.need("cert", path_in(base))?;
                let key = f.need("key", path_in(base))?;
                f.done()?;
                f.empty()?;
                TlsSource::Files { cert, key }
            }
            (Some(_), Some(f)) => {
                return Err(f.err("a domain has <self-signed/> or <files>, not both"))
            }
        };
        if domains.iter().any(|x: &Domain| x.name == dname) {
            return Err(d.err(format!("domain {dname:?} is listed twice")));
        }
        domains.push(Domain { name: dname, tls });
    }
    let pairing = match seq.maybe("pairing")? {
        Some(p) => pairing(&p)?,
        None => Pairing {
            approve: Approval::Manual,
            code_ttl: 60,
            token_ttl_days: 3650,
            lan_only: true,
            skew_seconds: 300,
            devices: Vec::new(),
        },
    };
    let mut units = Vec::new();
    for m in seq.many("manage")? {
        let u = m.need("unit", xml::ident)?;
        m.done()?;
        m.empty()?;
        if units.contains(&u) {
            return Err(m.err(format!("unit {u:?} is managed twice")));
        }
        units.push(u);
    }
    let mut faults = Vec::new();
    for f in seq.many("fault")? {
        faults.push(fault(&f)?);
    }
    seq.end()?;

    if listens.iter().any(|l| l.tls) && !domains.iter().any(|d| d.tls != TlsSource::None) {
        return Err(
            el.err("a tls=\"true\" listener needs at least one <domain> with a certificate")
        );
    }
    Ok(Server {
        origin: el.origin(),
        api_key: api_key.unwrap_or_else(|| hex_string(&derive_bytes(seed, &name, "api-key", 24))),
        name,
        build,
        host,
        listens,
        domains,
        pairing,
        units,
        faults,
        latency,
        clock_offset,
        state_dir,
    })
}

fn build(el: &El) -> Result<Build> {
    let profile_name = el.need("profile", xml::one_of(&profiles::names()))?;
    let profile = profiles::find(&profile_name).expect("checked by one_of");
    let feature = xml::one_of(profiles::ALL_FEATURES);
    let version = el.get("version", |s| {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() == 3 && parts.iter().all(|p| xml::unsigned::<u32>(p).is_ok()) {
            Ok(s.to_string())
        } else {
            Err("a version is three whole numbers, like 4.2.0".into())
        }
    })?;
    let commit = el.get("commit", |s| {
        if (7..=40).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            Ok(s.to_string())
        } else {
            Err("a commit is 7 to 40 lowercase hex digits".into())
        }
    })?;
    let add = el.get("add", xml::list(&feature))?;
    let remove = el.get("remove", xml::list(&feature))?;
    let auth_versions = el.get(
        "auth-versions",
        xml::list(|s| xml::one_of(&["1", "2"])(s).map(|v| v.parse::<u8>().unwrap_or(1))),
    )?;
    let min_auth = el.get("min-auth-version", xml::one_of(&["1", "2"]))?;
    el.done()?;
    el.empty()?;
    let mut list = profile.features();
    for f in add.unwrap_or_default() {
        if list.contains(&f) {
            return Err(el.err(format!("add=: profile {profile_name} already has {f}")));
        }
        list.push(f);
    }
    for f in remove.unwrap_or_default() {
        let before = list.len();
        list.retain(|x| x != &f);
        if list.len() == before {
            return Err(el.err(format!("remove=: profile {profile_name} does not have {f}")));
        }
    }
    // Keep Breeze Core's own order, whatever order the XML used.
    list.sort_by_key(|f| profiles::ALL_FEATURES.iter().position(|x| x == f));
    let auth_versions = auth_versions.unwrap_or_else(|| profile.auth_versions.to_vec());
    let min_auth_version = min_auth.map(|v| v.parse().unwrap_or(1)).unwrap_or(1);
    if !auth_versions.contains(&min_auth_version) {
        return Err(el.err("min-auth-version= must be one of auth-versions="));
    }
    if auth_versions.contains(&2) != list.iter().any(|f| f == "ed25519_auth") {
        return Err(el.err("auth version 2 and the ed25519_auth feature go together"));
    }
    Ok(Build {
        profile: profile_name,
        name: "Breeze Core".into(),
        version: version.unwrap_or_else(|| profile.version.into()),
        commit: commit.unwrap_or_else(|| profile.commit.into()),
        features: list,
        auth_versions,
        min_auth_version,
    })
}

fn host(el: &El) -> Result<Host> {
    let h = Host {
        hostname: el.need("hostname", hostname)?,
        os: el
            .get("os", xml::text)?
            .unwrap_or_else(|| "AlmaLinux 9.6 (Sage Margay)".into()),
        kernel: el
            .get("kernel", xml::text)?
            .unwrap_or_else(|| "5.14.0-570.el9.x86_64".into()),
        arch: el
            .get(
                "arch",
                xml::one_of(&["x86_64", "aarch64", "armv7l", "riscv64", "ppc64le", "s390x"]),
            )?
            .unwrap_or_else(|| "x86_64".into()),
        init: el
            .get(
                "init",
                xml::one_of(&[
                    "systemd", "openrc", "runit", "s6", "sysv", "procd", "launchd", "windows",
                    "none",
                ]),
            )?
            .unwrap_or_else(|| "systemd".into()),
        cpu: el
            .get("cpu", xml::text)?
            .unwrap_or_else(|| "Mock CPU @ 2.4GHz".into()),
        cpus: el.get("cpus", xml::unsigned::<u32>)?.unwrap_or(4),
        memory_mb: el.get("memory-mb", xml::unsigned::<u64>)?.unwrap_or(4096),
        timezone: el
            .get("timezone", xml::text)?
            .unwrap_or_else(|| "UTC".into()),
        utc_offset_seconds: el.get("utc-offset", xml::signed_seconds)?.unwrap_or(0),
        container: el.get(
            "container",
            xml::one_of(&["docker", "podman", "lxc", "systemd-nspawn"]),
        )?,
    };
    el.done()?;
    el.empty()?;
    Ok(h)
}

fn default_host(name: &str) -> Host {
    Host {
        hostname: format!("{name}.mock"),
        os: "AlmaLinux 9.6 (Sage Margay)".into(),
        kernel: "5.14.0-570.el9.x86_64".into(),
        arch: "x86_64".into(),
        init: "systemd".into(),
        cpu: "Mock CPU @ 2.4GHz".into(),
        cpus: 4,
        memory_mb: 4096,
        timezone: "UTC".into(),
        utc_offset_seconds: 0,
        container: None,
    }
}

fn pairing(el: &El) -> Result<Pairing> {
    let approve = match el
        .get("approve", xml::one_of(&["manual", "auto", "never"]))?
        .as_deref()
    {
        Some("auto") => Approval::Auto,
        Some("never") => Approval::Never,
        _ => Approval::Manual,
    };
    let code_ttl = el
        .get("code-ttl", xml::duration)?
        .unwrap_or(Duration::from_secs(60));
    let token_ttl = el
        .get("token-ttl", xml::duration)?
        .unwrap_or(Duration::from_secs(3650 * 86400));
    let lan_only = el.get("lan-only", xml::boolean)?.unwrap_or(true);
    let skew = el
        .get("skew", xml::duration)?
        .unwrap_or(Duration::from_secs(300));
    el.done()?;
    if code_ttl.as_secs() < 5 {
        return Err(el.err("code-ttl= is at least 5s"));
    }
    let mut seq = Seq::new(el, &["device"])?;
    let mut devices = Vec::new();
    for d in seq.many("device")? {
        let label = d.need("label", xml::text)?;
        let token = d.need("token", |s| {
            if (32..=128).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                Ok(s.to_string())
            } else {
                Err("a token is 32 to 128 characters of A-Z, a-z, 0-9, - and _".into())
            }
        })?;
        d.done()?;
        d.empty()?;
        devices.push(PresetDevice { label, token });
    }
    seq.end()?;
    Ok(Pairing {
        approve,
        code_ttl: code_ttl.as_secs(),
        token_ttl_days: (token_ttl.as_secs() / 86400).max(1) as i64,
        lan_only,
        skew_seconds: skew.as_secs(),
        devices,
    })
}

fn fault(el: &El) -> Result<Fault> {
    let path = el.need("path", |s| {
        let prefix = s.strip_suffix('*').unwrap_or(s);
        if !prefix.starts_with('/')
            || prefix.contains('*')
            || s.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err("a path starts with / and may end in * to match a prefix".into());
        }
        Ok(s.to_string())
    })?;
    let f = Fault {
        method: el.get(
            "method",
            xml::one_of(&["GET", "POST", "PUT", "PATCH", "DELETE"]),
        )?,
        path,
        rate: el.get("rate", xml::rate)?.unwrap_or(1.0),
        status: el
            .get("status", |s| {
                let v: u16 = xml::unsigned(s)?;
                if (100..=599).contains(&v) {
                    Ok(v)
                } else {
                    Err("an HTTP status is 100 to 599".into())
                }
            })?
            .unwrap_or(503),
        body: el.get("body", xml::text)?,
        delay: el.get("delay", xml::duration)?.unwrap_or_default(),
        hang_up: el.get("hang-up", xml::boolean)?.unwrap_or(false),
    };
    el.done()?;
    el.empty()?;
    if f.hang_up && f.body.is_some() {
        return Err(el.err("hang-up=\"true\" sends nothing, so body= would never be seen"));
    }
    Ok(f)
}

// ----------------------------------------------------------- cross-checks

/// Everything that needs the whole blueprint: references, uniqueness, and
/// addresses that must not collide.
fn check(root: &El, bp: &Blueprint) -> Result<()> {
    let _ = root;
    let mut rooms: BTreeMap<String, &Room> = BTreeMap::new();
    for f in &bp.building.floors {
        for r in &f.rooms {
            if let Some(first) = rooms.insert(r.id.clone(), r) {
                return Err(r.origin.err(format!(
                    "room id {:?} is already used at line {}",
                    r.id, first.origin.line
                )));
            }
        }
    }
    let mut keys: BTreeMap<String, &Unit> = BTreeMap::new();
    let mut ids: BTreeMap<u64, &Unit> = BTreeMap::new();
    let mut sockets: BTreeMap<(Ipv4Addr, u16), &Unit> = BTreeMap::new();
    let mut nicknames: BTreeMap<String, &Unit> = BTreeMap::new();
    for u in &bp.units {
        if let Some(first) = keys.insert(u.key.clone(), u) {
            return Err(u.origin.err(format!(
                "unit key {:?} is used twice (first at line {})",
                u.key, first.origin.line
            )));
        }
        if let Some(first) = ids.insert(u.id, u) {
            return Err(u.origin.err(format!(
                "id {} is already unit {:?}'s (line {})",
                u.id, first.key, first.origin.line
            )));
        }
        if let Some(n) = &u.identity.nickname {
            if let Some(first) = nicknames.insert(n.clone(), u) {
                return Err(u.origin.err(format!(
                    "nickname {n:?} is already unit {:?}'s (line {})",
                    first.key, first.origin.line
                )));
            }
        }
        let seg = bp
            .segments
            .iter()
            .find(|s| s.name == u.segment)
            .ok_or_else(|| {
                u.origin.err(format!(
                    "segment {:?} is not one <network> defines{}",
                    u.segment,
                    xml::suggest(
                        &u.segment,
                        &bp.segments
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                    )
                ))
            })?;
        if !seg.contains(u.ip) {
            return Err(u.origin.err(format!(
                "ip {} is outside its segment {} ({})",
                u.ip,
                seg.name,
                seg.cidr()
            )));
        }
        let hostmask = !(u32::MAX << (32 - seg.prefix));
        let host = u32::from(u.ip) & hostmask;
        if host == 0 || host == hostmask {
            return Err(u.origin.err(format!(
                "{} is the network or broadcast address of {}",
                u.ip,
                seg.cidr()
            )));
        }
        if let Some(first) = sockets.insert((u.ip, u.protocol.port), u) {
            return Err(u.origin.err(format!(
                "{}:{} is already unit {:?}'s (line {})",
                u.ip, u.protocol.port, first.key, first.origin.line
            )));
        }
        // A capability set no real unit could report is a blueprint error, not
        // something to discover when Breeze Core first asks.
        crate::lan::codec::capability_pages(&u.capabilities).map_err(|e| u.origin.err(e))?;
        if let Some(r) = &u.room {
            if !rooms.contains_key(r) {
                return Err(u.origin.err(format!(
                    "room {r:?} is not one <building> defines{}",
                    xml::suggest(r, &rooms.keys().map(String::as_str).collect::<Vec<_>>())
                )));
            }
        }
    }
    let mut names: BTreeMap<String, &Server> = BTreeMap::new();
    let mut domains: BTreeMap<String, &Server> = BTreeMap::new();
    let mut shared: BTreeMap<(IpAddr, u16), Vec<&Server>> = BTreeMap::new();
    for s in &bp.servers {
        if let Some(first) = names.insert(s.name.clone(), s) {
            return Err(s.origin.err(format!(
                "server name {:?} is used twice (first at line {})",
                s.name, first.origin.line
            )));
        }
        for u in &s.units {
            if !keys.contains_key(u) {
                return Err(s.origin.err(format!(
                    "manages unit {u:?}, which no <unit> defines{}",
                    xml::suggest(u, &keys.keys().map(String::as_str).collect::<Vec<_>>())
                )));
            }
        }
        for d in &s.domains {
            if let Some(first) = domains.insert(d.name.clone(), s) {
                return Err(s.origin.err(format!(
                    "domain {:?} is already server {:?}'s (line {})",
                    d.name, first.name, first.origin.line
                )));
            }
        }
        for l in &s.listens {
            shared.entry((l.address, l.port)).or_default().push(s);
        }
    }
    for ((addr, port), servers) in &shared {
        if servers.len() > 1 {
            if let Some(s) = servers.iter().find(|s| s.domains.is_empty()) {
                return Err(s.origin.err(format!(
                    "{addr} port {port} is shared by {} servers, so each needs a <domain> to be told apart, and this one has none",
                    servers.len()
                )));
            }
            let tls = |s: &Server| {
                s.listens
                    .iter()
                    .any(|l| l.address == *addr && l.port == *port && l.tls)
            };
            if servers.iter().any(|s| tls(s)) && servers.iter().any(|s| !tls(s)) {
                let plain = servers.iter().find(|s| !tls(s)).expect("checked");
                return Err(plain.origin.err(format!(
                    "{addr} port {port} is TLS for another server and plain for this one"
                )));
            }
        }
        for u in &bp.units {
            if IpAddr::V4(u.ip) == *addr && u.protocol.port == *port {
                return Err(servers[0].origin.err(format!(
                    "listener {addr}:{port} collides with unit {:?} (line {})",
                    u.key, u.origin.line
                )));
            }
        }
    }
    if let Some(c) = &bp.console {
        if let Some(servers) = shared.get(&(c.address, c.port)) {
            return Err(servers[0].origin.err(format!(
                "listener {}:{} is also the <console>'s",
                c.address, c.port
            )));
        }
    }
    Ok(())
}

// ------------------------------------------------------------- derivation
//
// "auto" values are derived from the seed and the thing's own name, never
// drawn at random: a Breeze Core config exported from a blueprint keeps
// working against the next run of the same blueprint.

pub fn derive_bytes(seed: u64, name: &str, what: &str, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut counter = 0u32;
    while out.len() < n {
        let mut h = Sha256::new();
        h.update(b"breeze-mock\0");
        h.update(seed.to_le_bytes());
        h.update(name.as_bytes());
        h.update([0]);
        h.update(what.as_bytes());
        h.update(counter.to_le_bytes());
        out.extend_from_slice(&h.finalize());
        counter += 1;
    }
    out.truncate(n);
    out
}

/// Fifteen digits starting 15, like the ids real units carry.
fn derive_id(seed: u64, key: &str) -> u64 {
    let b = derive_bytes(seed, key, "id", 8);
    let n = u64::from_le_bytes(b.try_into().expect("8 bytes"));
    150_000_000_000_000 + n % 10_000_000_000_000
}

fn derive_serial(seed: u64, key: &str, device_type: u8) -> String {
    let b = derive_bytes(seed, key, "serial", 8);
    format!(
        "000000P0000000Q1{device_type:02X}{}",
        b.iter().map(|x| format!("{x:02X}")).collect::<String>()
    )[..32]
        .to_string()
}

pub fn hex_string(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
