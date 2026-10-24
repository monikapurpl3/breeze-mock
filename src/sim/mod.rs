//! The simulated world: weather, rooms and units, stepped on a clock.
//!
//! One `World` behind one mutex. The ticker thread advances the physics; the
//! LAN emulator and the mock servers read and change units through the same
//! lock, so a unit controlled over the Midea protocol and read back through a
//! mock Breeze Core are one unit, not two copies.

pub mod rng;
pub mod thermal;
pub mod unit;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Timelike;

use crate::blueprint::{Blueprint, Room};
use crate::lan::codec::{Command, Report};
use rng::Rng;
use unit::UnitSim;

pub type Shared = Arc<Mutex<World>>;

#[derive(Debug, Clone)]
pub struct RoomState {
    pub spec: Room,
    pub temperature: f64,
    /// Made up for a unit placed in no room.
    pub private: bool,
}

pub struct World {
    pub name: String,
    pub time_scale: f64,
    pub tick: Duration,
    weather: crate::blueprint::Weather,
    /// Simulated seconds since the run started.
    pub elapsed: f64,
    /// Time of day, in hours, when the run started.
    start_hour: f64,
    last_tick: Instant,
    pub rooms: Vec<RoomState>,
    pub units: Vec<UnitSim>,
    pub rng: Rng,
    /// Set from the console to hold the outdoor temperature.
    pub outdoor_override: Option<f64>,
}

impl World {
    pub fn new(bp: &Blueprint) -> Self {
        let now = chrono::Local::now();
        let start_hour =
            now.hour() as f64 + now.minute() as f64 / 60.0 + now.second() as f64 / 3600.0;
        Self::starting_at(bp, start_hour)
    }

    /// A world whose clock starts at a given time of day, for tests.
    pub fn starting_at(bp: &Blueprint, start_hour: f64) -> Self {
        let mut rooms: Vec<RoomState> = bp
            .building
            .floors
            .iter()
            .flat_map(|f| f.rooms.iter())
            .map(|r| RoomState {
                spec: r.clone(),
                temperature: r.start_c,
                private: false,
            })
            .collect();
        let mut units = Vec::new();
        for u in &bp.units {
            let room = match &u.room {
                Some(id) => rooms
                    .iter()
                    .position(|r| &r.spec.id == id)
                    .expect("checked by the blueprint"),
                None => {
                    // A unit in no room still needs air to act on.
                    rooms.push(RoomState {
                        spec: Room {
                            origin: u.origin.clone(),
                            id: format!("{}-room", u.key),
                            name: format!("{}'s room", u.identity.name),
                            area_m2: 20.0,
                            height_m: 2.5,
                            loss_w_per_k: 3.5 * 20.0,
                            capacity_j_per_k: 20.0 * 2.5 * 1.2 * 1005.0 * 8.0,
                            internal_gain_w: 0.0,
                            sun_gain_w: 0.0,
                            start_c: 21.0,
                        },
                        temperature: 21.0,
                        private: true,
                    });
                    rooms.len() - 1
                }
            };
            units.push(UnitSim::new(u.clone(), room, 0.0));
        }
        Self {
            name: bp.name.clone(),
            time_scale: bp.environment.time_scale,
            tick: bp.environment.tick,
            weather: bp.environment.weather.clone(),
            elapsed: 0.0,
            start_hour,
            last_tick: Instant::now(),
            rooms,
            units,
            rng: Rng::new(bp.seed),
            outdoor_override: None,
        }
    }

    /// Simulated time of day, in hours.
    pub fn hour(&self) -> f64 {
        (self.start_hour + self.elapsed / 3600.0).rem_euclid(24.0)
    }

    pub fn outdoor(&self) -> f64 {
        self.outdoor_override
            .unwrap_or_else(|| thermal::outdoor(&self.weather, self.hour()))
    }

    /// Advance the physics by `real` seconds of wall time.
    pub fn advance(&mut self, real: f64) {
        let mut remaining = real * self.time_scale;
        while remaining > 0.0 {
            let dt = remaining.min(thermal::MAX_STEP);
            remaining -= dt;
            let hour = self.hour();
            let outdoor = self.outdoor();
            let mut per_room = vec![0.0; self.rooms.len()];
            for u in &mut self.units {
                let t = self.rooms[u.room].temperature;
                per_room[u.room] += u.output(t, outdoor, self.elapsed);
            }
            for (i, r) in self.rooms.iter_mut().enumerate() {
                r.temperature =
                    thermal::step(&r.spec, r.temperature, outdoor, hour, per_room[i], dt);
            }
            self.elapsed += dt;
        }
        let at = Instant::now();
        for i in 0..self.units.len() {
            let truth = self.truth(i);
            self.units[i].remember(at, truth);
        }
    }

    /// Step from the wall clock: however long it has really been since the
    /// last tick.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let real = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        self.advance(real);
    }

    pub fn by_id(&self, id: u64) -> Option<usize> {
        self.units.iter().position(|u| u.def.id == id)
    }

    pub fn by_key(&self, key: &str) -> Option<usize> {
        self.units
            .iter()
            .position(|u| u.def.key == key || u.def.identity.nickname.as_deref() == Some(key))
    }

    fn truth(&mut self, i: usize) -> Report {
        let t = self.rooms[self.units[i].room].temperature;
        let outdoor = self.outdoor();
        let u = &self.units[i];
        u.fresh_report(t, outdoor, &mut self.rng)
    }

    /// What unit `i` reports when asked now: the truth, or older if it lags.
    pub fn report(&mut self, i: usize) -> Report {
        let truth = self.truth(i);
        self.units[i].lagged(Instant::now(), truth)
    }

    /// Apply a command to unit `i`; the reply is the resulting state, which is
    /// what a real unit echoes. Lag never applies to a command's own reply:
    /// the unit answers with what it just did.
    pub fn control(&mut self, i: usize, c: &Command) -> (Report, Vec<&'static str>) {
        let refused = self.units[i].apply(c, self.elapsed);
        (self.truth(i), refused)
    }

    pub fn offline(&self, i: usize) -> bool {
        self.units[i].offline_at(self.hour())
    }
}

/// Keep the world moving until the process ends.
pub fn spawn_ticker(world: Shared) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        let tick = world
            .lock()
            .map(|w| w.tick)
            .unwrap_or(Duration::from_secs(1));
        std::thread::sleep(tick);
        if let Ok(mut w) = world.lock() {
            w.tick();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use breeze_proto::ac::types::{Mode, SwingMode};

    fn world(extra_unit: &str, epithets: &str) -> World {
        let text = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<blueprint xmlns="urn:breeze-mock:blueprint:1" version="1" name="t" seed="3">
  <environment><constant outdoor="0"/></environment>
  <building name="B"><floor name="F"><room id="r" name="R" area="14" start="18"/></floor></building>
  <network><segment name="lo" cidr="127.66.0.0/24" mode="loopback"/></network>
  <unit key="u" segment="lo" ip="127.66.0.2" room="r">
    <epithets name="U">{epithets}</epithets>
    <v3 token="auto" key="auto"/>
    {extra_unit}
  </unit>
</blueprint>
"#
        );
        let bp = crate::blueprint::parse(std::path::Path::new("sim-test.xml"), &text).unwrap();
        World::starting_at(&bp, 12.0)
    }

    fn command(w: &World, f: impl FnOnce(&mut Command)) -> Command {
        let l = &w.units[0].live;
        let mut c = Command {
            power: l.power,
            beep: false,
            mode: Some(l.mode),
            mode_raw: l.mode as u8,
            target: l.target,
            fan: l.fan,
            swing: Some(l.swing),
            swing_raw: l.swing as u8,
            eco: l.eco,
            turbo: l.turbo,
            sleep: l.sleep,
            fahrenheit: l.fahrenheit,
            purifier: false,
            aux_heat: false,
            independent_aux_heat: false,
            follow_me: false,
            freeze_protection: false,
            humidity: l.humidity,
        };
        f(&mut c);
        c.mode_raw = c.mode.map(|m| m as u8).unwrap_or(c.mode_raw);
        c.swing_raw = c.swing.map(|s| s as u8).unwrap_or(c.swing_raw);
        c
    }

    #[test]
    fn heating_warms_the_room_and_off_lets_it_cool() {
        let mut w = world(r#"<state power="on" mode="heat" target="22"/>"#, "");
        w.advance(3600.0);
        let warm = w.rooms[0].temperature;
        assert!(warm > 21.0, "an hour of heating from 18: {warm}");
        let c = command(&w, |c| c.power = false);
        w.control(0, &c);
        w.advance(3600.0);
        assert!(w.rooms[0].temperature < warm, "off, at 0 outside, it cools");
    }

    #[test]
    fn a_warming_heater_holds_its_flaps_and_then_lets_go() {
        let mut w = world(
            r#"<state power="off" mode="heat" target="22"/>"#,
            "<warmup-lock for=\"3m\"/>",
        );
        let on = command(&w, |c| c.power = true);
        w.control(0, &on);
        let flap = command(&w, |c| c.swing = Some(SwingMode::Vertical));
        let (report, refused) = w.control(0, &flap);
        assert_eq!(report.swing, SwingMode::Off);
        assert_eq!(refused, ["swing_mode"]);
        w.advance(200.0);
        let (report, refused) = w.control(0, &flap);
        assert_eq!(report.swing, SwingMode::Vertical, "warm now");
        assert!(refused.is_empty());
    }

    #[test]
    fn eco_outside_cool_and_flaps_while_off_are_refused_by_stubborn_units() {
        let mut w = world(
            r#"<state power="off" mode="heat" target="22"/>"#,
            "<eco-cool-only/><no-flaps-when-off/>",
        );
        let c = command(&w, |c| {
            c.eco = true;
            c.swing = Some(SwingMode::Vertical);
        });
        let (_, refused) = w.control(0, &c);
        assert_eq!(refused, ["swing_mode", "eco"]);
        let c = command(&w, |c| {
            c.power = true;
            c.mode = Some(Mode::Cool);
            c.eco = true;
        });
        let (report, refused) = w.control(0, &c);
        assert!(report.eco && refused.is_empty());
    }

    #[test]
    fn modes_and_axes_a_unit_lacks_are_ignored() {
        let mut w = world(
            r#"<capabilities modes="cool fan-only" swing="vertical" fan="low medium high auto"/>
               <state power="on" mode="cool" target="24"/>"#,
            "",
        );
        let c = command(&w, |c| {
            c.mode = Some(Mode::Heat);
            c.swing = Some(SwingMode::Both);
            c.fan = 20;
        });
        let (report, refused) = w.control(0, &c);
        assert_eq!(report.mode, Mode::Cool);
        assert_eq!(report.swing, SwingMode::Off);
        assert_eq!(refused, ["operational_mode", "fan_speed", "swing_mode"]);
    }

    #[test]
    fn sleepy_windows_wrap_midnight() {
        let w = world("", r#"<sleepy from="23:00" to="01:00"/>"#);
        assert!(w.units[0].offline_at(23.5));
        assert!(w.units[0].offline_at(0.5));
        assert!(!w.units[0].offline_at(12.0));
    }
}
