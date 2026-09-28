//! Breeze Core's own device client against emulated units, over real TCP.
//!
//! breeze-device is the code Breeze Core uses to talk to air conditioners, so
//! these are the strongest evidence there is that a real Breeze Core can
//! drive a mock unit: the same handshake, session, framing, retries and
//! parsing, unchanged.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use breeze_device::{Device, DeviceError, Timing, UnitConfig};
use breeze_mock::blueprint::{self, Blueprint};
use breeze_mock::lan::device;
use breeze_mock::sim::{Shared, World};
use breeze_proto::ac::command::Setpoint;
use breeze_proto::ac::types::{FanSpeed, Mode, SwingMode};
use breeze_proto::lan::LanError;

fn blueprint(name: &str, unit: &str) -> Blueprint {
    let dir = std::env::temp_dir().join(format!("bm-lan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.xml"));
    std::fs::write(
        &path,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<blueprint xmlns="urn:breeze-mock:blueprint:1" version="1" name="{name}" seed="5">
  <environment><constant outdoor="3"/></environment>
  <network><segment name="lo" cidr="127.0.0.0/24" mode="loopback"/></network>
  {unit}
</blueprint>
"#
        ),
    )
    .unwrap();
    blueprint::load(&path).unwrap()
}

/// Start the blueprint's first unit on a free loopback port.
fn start(bp: &Blueprint) -> (Shared, SocketAddr) {
    let world: Shared = Arc::new(Mutex::new(World::starting_at(bp, 12.0)));
    let addr = device::start(
        world.clone(),
        0,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
    )
    .unwrap();
    (world, addr)
}

fn client(
    bp: &Blueprint,
    addr: SocketAddr,
    token: Option<Vec<u8>>,
    key: Option<[u8; 32]>,
) -> Device {
    let u = &bp.units[0];
    Device::new(UnitConfig {
        id: u.id,
        name: u.identity.name.clone(),
        ip: addr.ip(),
        port: addr.port(),
        token: token.or_else(|| Some(u.protocol.token.clone())),
        key: key.or(Some(u.protocol.key)),
    })
    .with_timing(Timing {
        settle: Duration::from_millis(50),
        reply_timeout: Duration::from_millis(400),
        sends: 4,
        linger: Duration::from_millis(50),
    })
}

const PLAIN: &str = r#"<unit key="u" segment="lo" ip="127.0.0.1">
    <epithets name="Bench"/>
    <v3 token="auto" key="auto"/>
    <capabilities modes="auto cool dry heat fan-only" swing="vertical" fan="low medium high auto" eco="true" turbo="cool"/>
    <state power="on" mode="cool" target="23.5" fan="auto" swing="off"/>
  </unit>"#;

#[test]
fn breeze_core_reads_state_and_capabilities() {
    let bp = blueprint("read", PLAIN);
    let (world, addr) = start(&bp);
    let mut d = client(&bp, addr, None, None);
    let s = d.refresh().expect("state query");
    assert!(s.power_on);
    assert_eq!(s.mode, Some(Mode::Cool));
    assert_eq!(s.target_temperature, 23.5);
    assert_eq!(s.fan_speed, FanSpeed::AUTO);
    assert!(s.indoor_temperature.is_some() && s.outdoor_temperature.is_some());

    let caps = d.capabilities().expect("capabilities");
    assert_eq!(
        caps.operational_modes(),
        ["FAN_ONLY", "DRY", "COOL", "HEAT", "AUTO"]
    );
    assert_eq!(caps.swing_modes(), ["OFF", "VERTICAL"]);
    assert!(caps.eco && caps.turbo_cool && !caps.turbo_heat);

    let w = world.lock().unwrap();
    let st = &w.units[0].stats;
    assert_eq!(
        (st.handshakes, st.queries, st.capability_queries),
        (1, 1, 1)
    );
}

#[test]
fn breeze_core_controls_the_unit_and_sees_what_it_kept() {
    let bp = blueprint("control", PLAIN);
    let (world, addr) = start(&bp);
    let mut d = client(&bp, addr, None, None);
    let state = d.refresh().unwrap();
    let mut sp = Setpoint::from_state(&state);
    sp.mode = Mode::Heat;
    sp.target_temperature = 21.0;
    sp.fan_speed = FanSpeed::HIGH;
    // A vertical-only unit cannot do both flaps: kept as it was.
    sp.swing_mode = SwingMode::Both;
    let after = d.apply(&sp).expect("control");
    assert_eq!(after.mode, Some(Mode::Heat));
    assert_eq!(after.target_temperature, 21.0);
    assert_eq!(after.fan_speed, FanSpeed::HIGH);
    assert_eq!(after.swing_mode, Some(SwingMode::Off));
    let w = world.lock().unwrap();
    assert_eq!(
        w.units[0].live.mode,
        Mode::Heat,
        "the simulation took the command"
    );
    assert_eq!(w.units[0].last_refused, ["swing_mode"]);
}

#[test]
fn the_wrong_token_or_key_is_refused_at_the_handshake() {
    let bp = blueprint("creds", PLAIN);
    let (world, addr) = start(&bp);
    // A token that is not the unit's: the unit answers with an error packet.
    let mut wrong_token = client(&bp, addr, Some(vec![7u8; 64]), None);
    let e = wrong_token.refresh().unwrap_err();
    assert!(
        matches!(e, DeviceError::Protocol(LanError::DeviceError)),
        "{e:?}"
    );
    assert_eq!(world.lock().unwrap().units[0].stats.handshakes_refused, 1);
    // The right token with the wrong key: the unit accepts, and the client's
    // own check of the reply catches it -- as with a real unit.
    let mut wrong_key = client(&bp, addr, None, Some([9u8; 32]));
    let e = wrong_key.refresh().unwrap_err();
    assert!(
        matches!(e, DeviceError::Protocol(LanError::Rejected)),
        "{e:?}"
    );
}

#[test]
fn a_flaky_unit_still_works_because_breeze_core_resends() {
    let bp = blueprint(
        "flaky",
        &PLAIN.replace(
            r#"<epithets name="Bench"/>"#,
            r#"<epithets name="Bench"><flaky rate="1/2"/></epithets>"#,
        ),
    );
    let (world, addr) = start(&bp);
    let mut d = client(&bp, addr, None, None);
    let mut ok = 0;
    for _ in 0..8 {
        if d.refresh().is_ok() {
            ok += 1;
        }
    }
    assert!(
        ok >= 6,
        "only {ok} of 8 reads got through a 1-in-2 drop rate with resends"
    );
    assert!(
        world.lock().unwrap().units[0].stats.dropped > 0,
        "some requests were really dropped"
    );
}

#[test]
fn an_offline_unit_does_not_answer() {
    let bp = blueprint("offline", PLAIN);
    let (world, addr) = start(&bp);
    world.lock().unwrap().units[0].forced_offline = true;
    let mut d = client(&bp, addr, None, None);
    assert!(d.refresh().is_err());
    world.lock().unwrap().units[0].forced_offline = false;
    let mut d = client(&bp, addr, None, None);
    assert!(d.refresh().is_ok(), "and answers again once back");
}
