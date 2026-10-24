//! What a blueprint describes, once parsed and resolved.
//!
//! Everything here is plain data with defaults already applied: models merged
//! into units, `auto` ids, tokens and keys derived, references checked. Nothing
//! downstream has to know that a value was left out of the XML.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use breeze_proto::ac::types::{Mode, SwingMode};

pub use super::xml::Origin;

/// The whole of one run: the world, the units in it and the servers that
/// manage them.
#[derive(Debug, Clone)]
pub struct Blueprint {
    pub name: String,
    /// Where it was read from, for messages and relative paths.
    pub source: PathBuf,
    /// Seeds everything derived: ids, tokens, keys, and the randomness of
    /// flaky units, so two runs of one blueprint behave the same.
    pub seed: u64,
    pub environment: Environment,
    pub building: Building,
    pub segments: Vec<Segment>,
    pub units: Vec<Unit>,
    pub servers: Vec<Server>,
    pub console: Option<Console>,
}

// ------------------------------------------------------------------ world

#[derive(Debug, Clone)]
pub struct Environment {
    /// Simulated seconds per real second. Only the physics runs faster:
    /// servers, timers and sessions keep the wall clock, since that is the
    /// clock the clients under test use.
    pub time_scale: f64,
    /// How often the simulation steps, in real time.
    pub tick: Duration,
    pub weather: Weather,
}

#[derive(Debug, Clone)]
pub enum Weather {
    /// One outdoor temperature, all day.
    Fixed(f64),
    /// A daily cycle: `mean ± swing/2`, warmest at `warmest_hour`.
    Daily {
        mean: f64,
        swing: f64,
        warmest_hour: f64,
    },
    /// Outdoor temperature at given hours, interpolated and wrapped around
    /// midnight.
    Points(Vec<(f64, f64)>),
}

/// Floors and rooms. A unit in a room heats or cools that room; a unit in no
/// room has a private one of default size.
#[derive(Debug, Clone, Default)]
pub struct Building {
    pub name: String,
    pub floors: Vec<Floor>,
}

#[derive(Debug, Clone)]
pub struct Floor {
    pub name: String,
    pub level: i32,
    pub rooms: Vec<Room>,
}

#[derive(Debug, Clone)]
pub struct Room {
    pub origin: Origin,
    pub id: String,
    pub name: String,
    pub area_m2: f64,
    pub height_m: f64,
    /// Heat lost to outdoors per degree of difference, in W/K.
    pub loss_w_per_k: f64,
    /// Heat that has to move to change the room by one degree, in J/K.
    pub capacity_j_per_k: f64,
    /// Constant gains -- people, appliances -- in W.
    pub internal_gain_w: f64,
    /// Peak solar gain at noon, in W; zero at night.
    pub sun_gain_w: f64,
    pub start_c: f64,
}

// --------------------------------------------------------------- network

#[derive(Debug, Clone)]
pub struct Segment {
    pub origin: Origin,
    pub name: String,
    pub network: Ipv4Addr,
    pub prefix: u8,
    pub mode: SegmentMode,
    /// Answer discovery hellos on this segment.
    pub discovery: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentMode {
    /// Addresses in 127.0.0.0/8, which Linux routes to `lo` without any
    /// configuration. No privileges; not scannable, because Breeze Core
    /// sweeps only private ranges.
    Loopback,
    /// A dummy interface created for the run and removed after it, carrying
    /// the segment's addresses. Needs CAP_NET_ADMIN.
    Dummy { interface: String },
    /// Extra addresses on an existing interface, so other machines on that
    /// LAN reach the units. Needs CAP_NET_ADMIN; removed after the run.
    Attach { interface: String },
    /// The addresses are already there; bind and do nothing else.
    Existing,
}

impl Segment {
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = if self.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - self.prefix)
        };
        u32::from(ip) & mask == u32::from(self.network) & mask
    }

    pub fn cidr(&self) -> String {
        format!("{}/{}", self.network, self.prefix)
    }
}

// ------------------------------------------------------------------ units

#[derive(Debug, Clone)]
pub struct Unit {
    pub origin: Origin,
    /// The blueprint's handle for this unit, used by rooms and servers.
    pub key: String,
    /// The 48-bit Midea device id.
    pub id: u64,
    pub identity: Identity,
    pub protocol: Protocol,
    pub segment: String,
    pub ip: Ipv4Addr,
    pub room: Option<String>,
    pub hardware: Hardware,
    pub capabilities: CapabilitySet,
    pub state: InitialState,
    /// As written, for display; their effect is already in `behaviour`.
    pub epithets: Vec<String>,
    pub behaviour: Behaviour,
    /// Which model blueprint it came from, if any.
    pub model: Option<String>,
}

/// Names and the identity a unit reports about itself.
#[derive(Debug, Clone)]
pub struct Identity {
    /// What Breeze Core and the apps show.
    pub name: String,
    pub nickname: Option<String>,
    /// Free words for grouping units; the console lists them.
    pub labels: Vec<String>,
    pub model: String,
    /// 32 characters, as discovery reports it.
    pub serial: String,
    /// `net_<type>_<last four of the serial>`; the type in the middle is what
    /// discovery reads the appliance type from.
    pub ssid: String,
    pub device_type: u8,
    pub firmware: String,
}

#[derive(Debug, Clone)]
pub struct Protocol {
    pub version: ProtocolVersion,
    pub port: u16,
    /// V3 only: the cloud token (64 bytes) and key (32 bytes).
    pub token: Vec<u8>,
    pub key: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolVersion {
    V2,
    V3,
}

#[derive(Debug, Clone)]
pub struct Hardware {
    pub cooling_w: f64,
    pub heating_w: f64,
    pub indoor_sensor: bool,
    pub outdoor_sensor: bool,
    /// Added to the room temperature before reporting it.
    pub indoor_offset: f64,
    pub outdoor_offset: f64,
}

/// What the unit says it can do, in the terms the XML uses. Turned into a
/// real `0xB5` reply by `lan::codec`, which refuses combinations the protocol
/// cannot express.
#[derive(Debug, Clone)]
pub struct CapabilitySet {
    pub modes: Vec<Mode>,
    pub swing_vertical: bool,
    pub swing_horizontal: bool,
    pub fan: Vec<FanStep>,
    pub fan_custom: bool,
    pub eco: bool,
    pub turbo_cool: bool,
    pub turbo_heat: bool,
    pub freeze_protection: bool,
    pub display_control: bool,
    pub humidity: bool,
    pub buzzer: bool,
    pub fahrenheit: bool,
    pub cool_range: (f64, f64),
    pub auto_range: (f64, f64),
    pub heat_range: (f64, f64),
    pub half_degrees: bool,
    /// Report the capability list over two queries, as newer units do.
    pub two_pages: bool,
    /// Records sent verbatim after the generated ones: `(id, value bytes)`.
    pub extra_records: Vec<(u16, Vec<u8>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FanStep {
    Silent,
    Low,
    Medium,
    High,
    Auto,
}

impl FanStep {
    pub fn name(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InitialState {
    pub power: bool,
    pub mode: Mode,
    pub target: f64,
    pub fan: u8,
    pub swing: SwingMode,
    pub eco: bool,
    pub turbo: bool,
    pub sleep: bool,
    pub fahrenheit: bool,
    pub display: bool,
    pub humidity: u8,
    pub error_code: u8,
}

/// How a unit misbehaves. Every default is "like a textbook unit"; the
/// epithets set these to what real units were measured doing.
#[derive(Debug, Clone, Default)]
pub struct Behaviour {
    /// Delay before every reply. A real unit takes about 0.72 s.
    pub reply_delay: Duration,
    /// Extra random delay on top, uniform in `0..jitter`.
    pub jitter: Duration,
    /// Fraction of requests swallowed without an answer; the connection stays
    /// usable, as it does on real units (about 1 in 30).
    pub drop_rate: f64,
    /// Close a connection after this long without a request (real: 30 s).
    pub idle_hangup: Option<Duration>,
    /// After switching on or into HEAT, hold the flaps (and refuse flap
    /// changes) until the coil is warm.
    pub heat_warmup: Option<Duration>,
    /// Refuse eco outside COOL.
    pub eco_cool_only: bool,
    /// Ignore flap changes while switched off.
    pub no_flaps_when_off: bool,
    /// Refuse turbo in these modes.
    pub no_turbo_in: Vec<Mode>,
    /// Times of day (simulated) when the unit does not answer at all.
    pub offline: Vec<(f64, f64)>,
    /// Refuse every handshake, as a unit re-paired to another account does.
    pub reject_handshake: bool,
    /// Ignore the first request after each handshake, as real units do for
    /// about a second.
    pub post_handshake_deafness: Duration,
    /// Drop the session this often, forcing a new handshake.
    pub session_lifetime: Option<Duration>,
    /// Random noise added to reported temperatures, in degrees.
    pub sensor_noise: f64,
    /// Report state that lags the true state by this long.
    pub report_lag: Duration,
}

// ---------------------------------------------------------------- servers

/// One mock Breeze Core.
#[derive(Debug, Clone)]
pub struct Server {
    pub origin: Origin,
    pub name: String,
    pub api_key: String,
    pub build: Build,
    pub host: Host,
    pub listens: Vec<Listen>,
    pub domains: Vec<Domain>,
    pub pairing: Pairing,
    /// Unit keys this server manages, in the order it lists them.
    pub units: Vec<String>,
    pub faults: Vec<Fault>,
    /// Added to every response's latency.
    pub latency: Duration,
    /// Where it keeps devices.json and the rest between runs. `None` keeps
    /// everything in memory.
    pub state_dir: Option<PathBuf>,
}

/// Which Breeze Core this pretends to be.
#[derive(Debug, Clone)]
pub struct Build {
    pub profile: String,
    pub name: String,
    pub version: String,
    pub commit: String,
    pub features: Vec<String>,
    pub auth_versions: Vec<u8>,
    pub min_auth_version: u8,
}

impl Build {
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

/// The machine the server claims to run on, as `/api/system` reports it.
#[derive(Debug, Clone)]
pub struct Host {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub init: String,
    pub cpu: String,
    pub cpus: u32,
    pub timezone: String,
    pub utc_offset_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct Listen {
    pub address: std::net::IpAddr,
    pub port: u16,
    pub tls: bool,
}

#[derive(Debug, Clone)]
pub struct Domain {
    pub name: String,
    pub tls: TlsSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TlsSource {
    None,
    /// Signed by the run's own certificate authority, which `breeze-mock
    /// export ca` writes out for clients to trust.
    SelfSigned,
    Files {
        cert: PathBuf,
        key: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct Pairing {
    pub approve: Approval,
    pub code_ttl: u64,
    pub token_ttl_days: i64,
    /// Admin routes (approve, device list, revoke) only from private addresses.
    pub lan_only: bool,
    pub skew_seconds: u64,
    /// Clients paired before the run starts, so a test can skip pairing.
    pub devices: Vec<PresetDevice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// Pairing codes wait for an admin, as on a real server.
    Manual,
    /// Every code is approved the moment it is issued.
    Auto,
    /// Every code expires unapproved.
    Never,
}

#[derive(Debug, Clone)]
pub struct PresetDevice {
    pub label: String,
    /// A v1 bearer token the test already knows.
    pub token: String,
}

/// A deliberate failure, matched per request.
#[derive(Debug, Clone)]
pub struct Fault {
    pub method: Option<String>,
    /// A path, or a prefix ending in `*`.
    pub path: String,
    pub rate: f64,
    pub status: u16,
    pub body: Option<String>,
    pub delay: Duration,
    /// Close the connection without answering.
    pub hang_up: bool,
}

/// The mock's own control API, for inspecting and poking a running world.
#[derive(Debug, Clone)]
pub struct Console {
    pub address: std::net::IpAddr,
    pub port: u16,
}
