//! One emulated unit: what it is set to, how it answers a command, what it
//! reports, and how much heat it moves.
//!
//! Commands are applied the way the firmware applies them, not the way a
//! client hopes: a unit restates its whole configuration on every command,
//! keeps what it cannot do as it was, and says nothing about it -- the only
//! evidence is the state it reports back. That is exactly what Breeze Core's
//! `not_applied` is computed from, so a refusal here shows up there.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use breeze_proto::ac::types::{FanSpeed, Mode, SwingMode};

use super::rng::Rng;
use crate::blueprint::{FanStep, Unit};
use crate::lan::codec::{Command, Report};

/// What the unit is set to right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
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
    pub purifier: bool,
    pub aux_heat: bool,
    pub independent_aux_heat: bool,
    pub follow_me: bool,
    pub freeze_protection: bool,
}

/// Counters, for the console and for tests that count round-trips.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Stats {
    pub connections: u64,
    pub handshakes: u64,
    pub handshakes_refused: u64,
    pub queries: u64,
    pub controls: u64,
    pub capability_queries: u64,
    pub discovery_replies: u64,
    pub dropped: u64,
    pub refused_fields: u64,
    pub beeps: u64,
}

#[derive(Debug, Clone)]
pub struct UnitSim {
    pub def: Unit,
    pub live: Live,
    /// Index into the world's rooms.
    pub room: usize,
    /// Simulated time at which a heating coil is warm; `None` when not heating
    /// or already warm.
    pub warm_at: Option<f64>,
    heating_since: f64,
    /// Held offline from the console, whatever the time of day.
    pub forced_offline: bool,
    pub stats: Stats,
    /// Recent reports, for units whose reports lag behind the truth.
    history: VecDeque<(Instant, Report)>,
    /// What the last command asked for that the unit did not do.
    pub last_refused: Vec<&'static str>,
    /// Heat moved in the last step, in W (negative when cooling).
    pub output_w: f64,
}

impl UnitSim {
    pub fn new(def: Unit, room: usize, now: f64) -> Self {
        let s = &def.state;
        let live = Live {
            power: s.power,
            mode: s.mode,
            target: s.target,
            fan: s.fan,
            swing: s.swing,
            eco: s.eco,
            turbo: s.turbo,
            sleep: s.sleep,
            fahrenheit: s.fahrenheit,
            display: s.display,
            humidity: s.humidity,
            error_code: s.error_code,
            purifier: false,
            aux_heat: false,
            independent_aux_heat: false,
            follow_me: false,
            freeze_protection: false,
        };
        // A unit that starts heating starts warm: the blueprint describes a
        // unit that has been running, not one that was just switched on.
        Self {
            def,
            live,
            room,
            warm_at: None,
            heating_since: now,
            forced_offline: false,
            stats: Stats::default(),
            history: VecDeque::new(),
            last_refused: Vec::new(),
            output_w: 0.0,
        }
    }

    pub fn heating(&self) -> bool {
        self.live.power && self.live.mode == Mode::Heat
    }

    pub fn warming_up(&self, now: f64) -> bool {
        self.heating() && self.warm_at.is_some_and(|t| now < t)
    }

    /// Whether the unit answers at all at this time of day.
    pub fn offline_at(&self, hour: f64) -> bool {
        self.forced_offline
            || self.def.behaviour.offline.iter().any(|&(from, to)| {
                if from < to {
                    (from..to).contains(&hour)
                } else {
                    hour >= from || hour < to
                }
            })
    }

    fn range(&self, mode: Mode) -> (f64, f64) {
        let c = &self.def.capabilities;
        match mode {
            Mode::Heat => c.heat_range,
            Mode::Auto => c.auto_range,
            _ => c.cool_range,
        }
    }

    fn fan_allowed(&self, fan: u8) -> bool {
        let c = &self.def.capabilities;
        if c.fan_custom {
            return (1..=100).contains(&fan) || fan == FanSpeed::AUTO.0;
        }
        let step = match fan {
            20 => FanStep::Silent,
            40 => FanStep::Low,
            60 => FanStep::Medium,
            80 => FanStep::High,
            102 => FanStep::Auto,
            // 100 is "full", which only a custom-speed unit has as a named step.
            _ => return false,
        };
        c.fan.contains(&step)
    }

    fn swing_allowed(&self, s: SwingMode) -> bool {
        let c = &self.def.capabilities;
        match s {
            SwingMode::Off => true,
            SwingMode::Vertical => c.swing_vertical,
            SwingMode::Horizontal => c.swing_horizontal,
            SwingMode::Both => c.swing_vertical && c.swing_horizontal,
        }
    }

    /// Apply a control command. Returns the fields it asked for and did not
    /// get, in the REST API's names.
    pub fn apply(&mut self, c: &Command, now: f64) -> Vec<&'static str> {
        let caps = self.def.capabilities.clone();
        let b = self.def.behaviour.clone();
        let was_heating = self.heating();

        self.live.power = c.power;
        if let Some(m) = c.mode.filter(|m| caps.modes.contains(m)) {
            self.live.mode = m;
        }

        let heating = self.heating();
        if heating && !was_heating {
            self.heating_since = now;
            self.warm_at = b.heat_warmup.map(|d| now + d.as_secs_f64());
        }
        if !heating {
            self.warm_at = None;
        }

        let (lo, hi) = self.range(self.live.mode);
        let mut target = c.target.clamp(lo, hi);
        if !caps.half_degrees {
            target = target.floor();
        }
        self.live.target = target;

        if self.fan_allowed(c.fan) {
            self.live.fan = c.fan;
        }

        // The flaps: held still while switched off (on units that do that)
        // and while a heating coil warms up, which is the firmware behaviour
        // that once looked exactly like a bug.
        let locked = (b.no_flaps_when_off && !self.live.power) || self.warming_up(now);
        if let Some(s) = c.swing.filter(|s| self.swing_allowed(*s)) {
            if !locked {
                self.live.swing = s;
            }
        }

        let eco_ok = caps.eco && (!b.eco_cool_only || self.live.mode == Mode::Cool);
        self.live.eco = c.eco && eco_ok;

        let turbo_ok = match self.live.mode {
            Mode::Cool => caps.turbo_cool,
            Mode::Heat => caps.turbo_heat,
            _ => caps.turbo_cool || caps.turbo_heat,
        } && !b.no_turbo_in.contains(&self.live.mode);
        self.live.turbo = c.turbo && turbo_ok;

        self.live.sleep = c.sleep;
        self.live.fahrenheit = c.fahrenheit && caps.fahrenheit;
        if caps.humidity {
            self.live.humidity = c.humidity.min(100);
        }
        self.live.freeze_protection = c.freeze_protection && caps.freeze_protection;
        self.live.purifier = c.purifier;
        self.live.aux_heat = c.aux_heat;
        self.live.independent_aux_heat = c.independent_aux_heat;
        self.live.follow_me = c.follow_me;
        if c.beep {
            self.stats.beeps += 1;
        }

        let mut refused = Vec::new();
        if c.mode_raw != self.live.mode as u8 {
            refused.push("operational_mode");
        }
        if c.target != self.live.target {
            refused.push("target_temperature");
        }
        if c.fan != self.live.fan {
            refused.push("fan_speed");
        }
        if c.swing_raw != self.live.swing as u8 {
            refused.push("swing_mode");
        }
        if c.eco != self.live.eco {
            refused.push("eco");
        }
        if c.turbo != self.live.turbo {
            refused.push("turbo");
        }
        self.stats.controls += 1;
        self.stats.refused_fields += refused.len() as u64;
        self.last_refused = refused.clone();
        refused
    }

    /// Heat moved into the room this instant, in W; negative when cooling.
    ///
    /// An inverter's behaviour, roughly: output in proportion to how far the
    /// room is from the setpoint, never below a minimum while running, and
    /// shaped by the fan, turbo, eco and -- when heating -- how cold it is
    /// outside and whether the coil is warm yet.
    pub fn output(&mut self, room: f64, outdoor: f64, now: f64) -> f64 {
        if self.warm_at.is_some_and(|t| now >= t) {
            self.warm_at = None;
        }
        let l = &self.live;
        if !l.power {
            self.output_w = 0.0;
            return 0.0;
        }
        let fan = if l.fan == FanSpeed::AUTO.0 {
            1.0
        } else {
            0.35 + 0.65 * (l.fan.min(100) as f64 / 100.0)
        };
        let turbo = if l.turbo { 1.25 } else { 1.0 };
        let modulate = |error: f64| {
            ((error + 0.5) / 1.5)
                .clamp(0.0, 1.0)
                .max(if error > -0.5 { 0.15 } else { 0.0 })
        };
        let mode = match l.mode {
            Mode::Auto if room < l.target - 0.5 => Mode::Heat,
            Mode::Auto if room > l.target + 0.5 => Mode::Cool,
            Mode::Auto => Mode::FanOnly,
            m => m,
        };
        let hw = &self.def.hardware;
        let w = match mode {
            Mode::Cool | Mode::Dry => {
                let eco = if l.eco { 0.75 } else { 1.0 };
                let dry = if mode == Mode::Dry { 0.35 } else { 1.0 };
                -hw.cooling_w * fan * turbo * eco * dry * modulate(room - l.target)
            }
            Mode::Heat => {
                // A heat pump loses capacity as it gets colder outside.
                let cold = (1.0 + (outdoor - 7.0) * 0.02).clamp(0.5, 1.15);
                let ramp = match self.warm_at {
                    Some(t) if t > self.heating_since => {
                        0.2 + 0.8
                            * ((now - self.heating_since) / (t - self.heating_since))
                                .clamp(0.0, 1.0)
                    }
                    _ => 1.0,
                };
                hw.heating_w * fan * turbo * cold * ramp * modulate(l.target - room)
            }
            // A fan's motor ends up as heat in the room too.
            _ => 30.0,
        };
        self.output_w = w;
        w
    }

    /// The report the unit would send now, from the true state.
    pub fn fresh_report(&self, room: f64, outdoor: f64, rng: &mut Rng) -> Report {
        let hw = &self.def.hardware;
        let noise = self.def.behaviour.sensor_noise;
        let l = &self.live;
        Report {
            power: l.power,
            mode: l.mode,
            target: l.target,
            fan: l.fan,
            swing: l.swing,
            eco: l.eco,
            turbo: l.turbo,
            sleep: l.sleep,
            fahrenheit: l.fahrenheit,
            indoor: hw
                .indoor_sensor
                .then(|| room + hw.indoor_offset + rng.around(noise)),
            outdoor: hw
                .outdoor_sensor
                .then(|| outdoor + hw.outdoor_offset + rng.around(noise)),
            filter_alert: false,
            display: l.display,
            aux_heat: l.aux_heat,
            independent_aux_heat: l.independent_aux_heat,
            follow_me: l.follow_me,
            purifier: l.purifier,
            error_code: l.error_code,
            humidity: l.humidity,
            freeze_protection: l.freeze_protection,
        }
    }

    /// Record the truth, for a unit whose reports lag.
    pub fn remember(&mut self, at: Instant, report: Report) {
        let lag = self.def.behaviour.report_lag;
        if lag.is_zero() {
            return;
        }
        self.history.push_back((at, report));
        while self
            .history
            .front()
            .is_some_and(|(t, _)| at.duration_since(*t) > lag + Duration::from_secs(5))
        {
            self.history.pop_front();
        }
    }

    /// What a lagging unit reports: the newest remembered state at least `lag`
    /// old, or the truth when nothing is that old yet.
    pub fn lagged(&self, at: Instant, truth: Report) -> Report {
        let lag = self.def.behaviour.report_lag;
        if lag.is_zero() {
            return truth;
        }
        self.history
            .iter()
            .rev()
            .find(|(t, _)| at.duration_since(*t) >= lag)
            .map(|(_, r)| r.clone())
            .unwrap_or(truth)
    }
}
