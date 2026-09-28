//! The device side of the air-conditioner payloads.
//!
//! breeze-proto encodes commands and decodes reports, because Breeze Core is a
//! client. An emulated unit needs the inverse: decode commands, encode
//! reports and capability lists. Every function here is tested by feeding its
//! output to breeze-proto's own client-side decoder, so an emulated unit says
//! exactly what Breeze Core will read.

use breeze_proto::ac::types::{Mode, SwingMode};

use crate::blueprint::{CapabilitySet, FanStep};

/// What a unit reports in a `0xC0` state payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub power: bool,
    pub mode: Mode,
    pub target: f64,
    pub fan: u8,
    pub swing: SwingMode,
    pub eco: bool,
    pub turbo: bool,
    pub sleep: bool,
    pub fahrenheit: bool,
    pub indoor: Option<f64>,
    pub outdoor: Option<f64>,
    pub filter_alert: bool,
    pub display: bool,
    pub aux_heat: bool,
    pub independent_aux_heat: bool,
    pub follow_me: bool,
    pub purifier: bool,
    pub error_code: u8,
    pub humidity: u8,
    pub freeze_protection: bool,
}

/// A temperature as the protocol carries it: a half-degree byte biased by 50,
/// plus a tenths nibble. `None` is the documented 0xFF "no sensor".
///
/// The decoder truncates towards zero and then moves away from it by the
/// nibble, so a reading between 0 and -1 needs the -0.5 byte to keep its sign.
fn temperature_bytes(t: Option<f64>) -> (u8, u8) {
    let Some(t) = t else {
        return (0xFF, 0);
    };
    // Tenths are all the protocol has; clamp to what the byte can hold.
    let t = (t * 10.0).round() / 10.0;
    let t = t.clamp(-25.0, 102.0);
    let whole = t.trunc();
    let mut tenths = ((t - whole).abs() * 10.0).round() as u8;
    let mut whole = whole;
    if tenths >= 10 {
        tenths = 0;
        whole += t.signum();
    }
    let byte = if t < 0.0 && whole == 0.0 {
        49 // (49 - 50) / 2 = -0.5, which truncates to -0 and keeps the sign
    } else {
        (whole * 2.0 + 50.0) as u8
    };
    (byte, tenths)
}

/// Encode a state report payload (the frame layer adds the rest).
pub fn encode_report(r: &Report) -> Vec<u8> {
    let mut p = vec![0u8; 24];
    p[0] = 0xC0;
    p[1] = u8::from(r.power);
    let integral = r.target.trunc() as i32;
    let half = r.target - integral as f64 > 0.0;
    // The setpoint goes in byte 13 (biased by 12), which the decoder prefers,
    // and in byte 2's low nibble (biased by 16) as older firmware reports it.
    let primary = if (16..=31).contains(&integral) {
        ((integral - 16) as u8) & 0xF
    } else {
        0
    };
    p[2] = ((r.mode as u8 & 0x7) << 5) | if half { 0x10 } else { 0 } | primary;
    p[3] = r.fan & 0x7F;
    p[7] = r.swing as u8 & 0xF;
    p[8] = if r.turbo { 0x20 } else { 0 }
        | if r.independent_aux_heat { 0x40 } else { 0 }
        | if r.follow_me { 0x80 } else { 0 };
    p[9] = if r.eco { 0x10 } else { 0 }
        | if r.aux_heat { 0x08 } else { 0 }
        | if r.purifier { 0x20 } else { 0 };
    p[10] =
        u8::from(r.sleep) | if r.turbo { 0x02 } else { 0 } | if r.fahrenheit { 0x04 } else { 0 };
    let (indoor, indoor_tenths) = temperature_bytes(r.indoor);
    let (outdoor, outdoor_tenths) = temperature_bytes(r.outdoor);
    p[11] = indoor;
    p[12] = outdoor;
    p[13] = (((integral - 12) as u8) & 0x1F) | if r.filter_alert { 0x20 } else { 0 };
    p[14] = if r.display { 0x00 } else { 0x70 };
    p[15] = (outdoor_tenths << 4) | (indoor_tenths & 0xF);
    p[16] = r.error_code;
    p[19] = r.humidity & 0x7F;
    p[21] = if r.freeze_protection { 0x80 } else { 0 };
    p
}

/// A `0x40` control command, decoded. Enum fields keep the raw value too, so
/// a unit can refuse a mode it has never heard of rather than guess.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub power: bool,
    pub beep: bool,
    pub mode: Option<Mode>,
    pub mode_raw: u8,
    pub target: f64,
    pub fan: u8,
    pub swing: Option<SwingMode>,
    pub swing_raw: u8,
    pub eco: bool,
    pub turbo: bool,
    pub sleep: bool,
    pub fahrenheit: bool,
    pub purifier: bool,
    pub aux_heat: bool,
    pub independent_aux_heat: bool,
    pub follow_me: bool,
    pub freeze_protection: bool,
    pub humidity: u8,
}

/// Decode a control payload, starting at its `0x40` opcode.
pub fn decode_command(p: &[u8]) -> Result<Command, String> {
    if p.len() < 23 {
        return Err(format!(
            "a control command of {} bytes is too short",
            p.len()
        ));
    }
    if p[0] != 0x40 {
        return Err(format!("opcode 0x{:02X} is not a control command", p[0]));
    }
    let half = p[2] & 0x10 != 0;
    let primary = p[2] & 0xF;
    let alternate = p[18] & 0x1F;
    // The command puts 17-30 in the primary nibble and everything else in
    // byte 18; exactly one of them is set.
    let whole = if primary != 0 {
        primary as f64 + 16.0
    } else {
        alternate as f64 + 12.0
    };
    let mode_raw = (p[2] >> 5) & 0x7;
    let swing_raw = p[7] & 0xF;
    Ok(Command {
        power: p[1] & 0x01 != 0,
        beep: p[1] & 0x40 != 0,
        mode: Mode::from_wire(mode_raw),
        mode_raw,
        target: whole + if half { 0.5 } else { 0.0 },
        fan: p[3] & 0x7F,
        swing: SwingMode::from_wire(swing_raw),
        swing_raw,
        eco: p[9] & 0x80 != 0,
        turbo: p[8] & 0x20 != 0 || p[10] & 0x02 != 0,
        sleep: p[10] & 0x01 != 0,
        fahrenheit: p[10] & 0x04 != 0,
        purifier: p[9] & 0x20 != 0,
        aux_heat: p[9] & 0x08 != 0,
        independent_aux_heat: p[22] & 0x08 != 0,
        follow_me: p[8] & 0x80 != 0,
        freeze_protection: p[21] & 0x80 != 0,
        humidity: p[19] & 0x7F,
    })
}

/// Which capability query this is: the first page (`B5 01 00`) or the second
/// (`B5 01 01 01`).
pub fn capability_page(p: &[u8]) -> Option<usize> {
    match p {
        [0xB5, 0x01, 0x01, 0x01, ..] => Some(1),
        [0xB5, 0x01, 0x00, ..] | [0xB5, 0x01] => Some(0),
        _ => None,
    }
}

// ---------------------------------------------------------- capabilities
//
// Most capability records are one byte naming a *set* of features. These
// tables are breeze-proto's decoding tables turned around: for each value, what
// the decoder derives from it. The encoder picks the value whose derived set is
// exactly the one the blueprint asked for, and refuses a set no value gives --
// such a unit could not exist, and a mock of it would test nothing.

mod id {
    pub const FAN_SPEED_CONTROL: u16 = 0x0210;
    pub const PRESET_ECO: u16 = 0x0212;
    pub const PRESET_FREEZE_PROTECTION: u16 = 0x0213;
    pub const MODES: u16 = 0x0214;
    pub const SWING_MODES: u16 = 0x0215;
    pub const HUMIDITY: u16 = 0x021F;
    pub const PRESET_TURBO: u16 = 0x021A;
    pub const FAHRENHEIT: u16 = 0x0222;
    pub const DISPLAY_CONTROL: u16 = 0x0224;
    pub const TEMPERATURES: u16 = 0x0225;
    pub const BUZZER: u16 = 0x022C;
}

/// (heat, cool, dry, auto) for each MODES value that has no auxiliary-heat
/// modes, which the blueprint cannot ask for.
const MODE_VALUES: &[(u8, [bool; 4])] = &[
    (0, [false, true, true, true]),
    (1, [true, true, true, true]),
    (2, [true, false, false, true]),
    (3, [false, true, false, false]),
    (4, [true, true, false, false]),
    (5, [false, true, true, false]),
    (6, [true, true, true, false]),
    (7, [true, true, false, true]),
    (8, [false, true, false, true]),
    (12, [true, false, false, false]),
];

/// (silent, low, medium, high, auto) for each FAN_SPEED_CONTROL value except
/// 1, which is "any percentage".
const FAN_VALUES: &[(u8, [bool; 5])] = &[
    (3, [false, true, false, true, false]),
    (4, [false, true, false, true, true]),
    (5, [false, true, true, true, true]),
    (6, [true, true, true, true, true]),
    (7, [false, true, true, true, false]),
    (9, [true, true, false, true, true]),
];

fn words(set: &[bool], names: &[&str]) -> String {
    let w: Vec<&str> = set
        .iter()
        .zip(names)
        .filter(|(b, _)| **b)
        .map(|(_, n)| *n)
        .collect();
    if w.is_empty() {
        "none".into()
    } else {
        w.join(" ")
    }
}

fn modes_value(modes: &[Mode]) -> Result<u8, String> {
    let want = [
        modes.contains(&Mode::Heat),
        modes.contains(&Mode::Cool),
        modes.contains(&Mode::Dry),
        modes.contains(&Mode::Auto),
    ];
    if !modes.contains(&Mode::FanOnly) {
        return Err("every unit can move air, so modes= must include fan-only".into());
    }
    MODE_VALUES
        .iter()
        .find(|(_, set)| *set == want)
        .map(|(v, _)| *v)
        .ok_or_else(|| {
            let names = ["heat", "cool", "dry", "auto"];
            let options: Vec<String> = MODE_VALUES
                .iter()
                .map(|(_, s)| format!("\"{} fan-only\"", words(s, &names)))
                .collect();
            format!(
                "no Midea unit reports the modes {:?} plus fan-only; the protocol can say {}",
                words(&want, &names),
                options.join(", ")
            )
        })
}

fn fan_value(caps: &CapabilitySet) -> Result<u8, String> {
    let want = [
        caps.fan.contains(&FanStep::Silent),
        caps.fan.contains(&FanStep::Low),
        caps.fan.contains(&FanStep::Medium),
        caps.fan.contains(&FanStep::High),
        caps.fan.contains(&FanStep::Auto),
    ];
    let names = ["silent", "low", "medium", "high", "auto"];
    if caps.fan_custom {
        if want != [true; 5] {
            return Err(
                "a unit with fan-custom=\"true\" takes any speed, so it reports every named one: \
                        fan= must be \"silent low medium high auto\""
                    .into(),
            );
        }
        return Ok(1);
    }
    FAN_VALUES
        .iter()
        .find(|(_, set)| *set == want)
        .map(|(v, _)| *v)
        .ok_or_else(|| {
            let options: Vec<String> = FAN_VALUES.iter().map(|(_, s)| format!("\"{}\"", words(s, &names))).collect();
            format!(
                "no Midea unit reports the fan speeds {:?}; the protocol can say {} (or fan-custom=\"true\")",
                words(&want, &names),
                options.join(", ")
            )
        })
}

/// The capability reply, as one page or two, each a complete `0xB5` payload.
/// Fails if the blueprint asks for something the protocol cannot express.
pub fn capability_pages(caps: &CapabilitySet) -> Result<Vec<Vec<u8>>, String> {
    let swing = match (caps.swing_vertical, caps.swing_horizontal) {
        (true, true) => 1,
        (true, false) => 0,
        (false, true) => 3,
        (false, false) => 2,
    };
    let turbo = match (caps.turbo_cool, caps.turbo_heat) {
        (true, true) => 1,
        (true, false) => 0,
        (false, true) => 3,
        (false, false) => 2,
    };
    let half = |d: f64| (d * 2.0) as u8;
    let mut first: Vec<(u16, Vec<u8>)> = vec![
        (id::MODES, vec![modes_value(&caps.modes)?]),
        (id::SWING_MODES, vec![swing]),
        (id::FAN_SPEED_CONTROL, vec![fan_value(caps)?]),
        (id::PRESET_ECO, vec![u8::from(caps.eco)]),
        (id::PRESET_TURBO, vec![turbo]),
        (
            id::PRESET_FREEZE_PROTECTION,
            vec![u8::from(caps.freeze_protection)],
        ),
        (id::DISPLAY_CONTROL, vec![u8::from(caps.display_control)]),
        (id::HUMIDITY, vec![if caps.humidity { 2 } else { 0 }]),
        // Inverted in the vendor's table: 0 means Fahrenheit is available.
        (id::FAHRENHEIT, vec![if caps.fahrenheit { 0 } else { 1 }]),
        (id::BUZZER, vec![u8::from(caps.buzzer)]),
    ];
    let mut second: Vec<(u16, Vec<u8>)> = vec![(
        id::TEMPERATURES,
        vec![
            half(caps.cool_range.0),
            half(caps.cool_range.1),
            half(caps.auto_range.0),
            half(caps.auto_range.1),
            half(caps.heat_range.0),
            half(caps.heat_range.1),
            u8::from(caps.half_degrees),
        ],
    )];
    for (rid, value) in &caps.extra_records {
        if first.iter().chain(second.iter()).any(|(i, _)| i == rid) {
            return Err(format!(
                "record 0x{rid:04x} is one breeze-mock generates from the other attributes; set those instead"
            ));
        }
        second.push((*rid, value.clone()));
    }
    let page = |records: &[(u16, Vec<u8>)], more: bool| -> Vec<u8> {
        let mut p = vec![0xB5, records.len() as u8];
        for (rid, value) in records {
            p.extend_from_slice(&rid.to_le_bytes());
            p.push(value.len() as u8);
            p.extend_from_slice(value);
        }
        // The decoder reads the flag second from last.
        p.push(u8::from(more));
        p.push(0x00);
        p
    };
    if caps.two_pages {
        Ok(vec![page(&first, true), page(&second, false)])
    } else {
        first.append(&mut second);
        Ok(vec![page(&first, false)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use breeze_proto::ac::capabilities::Capabilities;
    use breeze_proto::ac::command::{self, Setpoint};
    use breeze_proto::ac::response::State;
    use breeze_proto::ac::types::FanSpeed;
    use breeze_proto::frame::{self, DeviceType};

    fn report() -> Report {
        Report {
            power: true,
            mode: Mode::Heat,
            target: 22.5,
            fan: 60,
            swing: SwingMode::Vertical,
            eco: false,
            turbo: true,
            sleep: false,
            fahrenheit: false,
            indoor: Some(21.3),
            outdoor: Some(-4.7),
            filter_alert: false,
            display: true,
            aux_heat: false,
            independent_aux_heat: false,
            follow_me: false,
            purifier: false,
            error_code: 0,
            humidity: 45,
            freeze_protection: false,
        }
    }

    #[test]
    fn a_report_reads_back_through_breeze_proto() {
        let s = State::parse(&encode_report(&report())).unwrap();
        assert!(s.power_on);
        assert_eq!(s.mode, Some(Mode::Heat));
        assert_eq!(s.target_temperature, 22.5);
        assert_eq!(s.fan_speed, FanSpeed(60));
        assert_eq!(s.swing_mode, Some(SwingMode::Vertical));
        assert!(s.turbo && !s.eco);
        assert_eq!(s.indoor_temperature, Some(21.3));
        assert_eq!(s.outdoor_temperature, Some(-4.7));
        assert_eq!(s.target_humidity, Some(45));
        assert!(s.display_on);
    }

    #[test]
    fn every_setpoint_and_temperature_survives_the_round_trip() {
        for t2 in 32..=60 {
            let mut r = report();
            r.target = t2 as f64 / 2.0;
            assert_eq!(
                State::parse(&encode_report(&r)).unwrap().target_temperature as f64,
                r.target
            );
        }
        // Tenths across the whole sensor range, including the -1..0 gap
        // where a naive encoding loses the sign.
        for tenths in -250..=600 {
            let t = tenths as f64 / 10.0;
            let mut r = report();
            r.indoor = Some(t);
            let got = State::parse(&encode_report(&r))
                .unwrap()
                .indoor_temperature
                .unwrap() as f64;
            assert!((got - t).abs() < 0.051, "{t} came back as {got}");
        }
        let mut r = report();
        r.indoor = None;
        r.outdoor = None;
        let s = State::parse(&encode_report(&r)).unwrap();
        assert_eq!((s.indoor_temperature, s.outdoor_temperature), (None, None));
    }

    #[test]
    fn a_command_from_breeze_proto_decodes_to_what_was_sent() {
        for target in [16.0f32, 16.5, 17.0, 22.5, 30.0] {
            let sp = Setpoint {
                power_on: true,
                mode: Mode::Cool,
                target_temperature: target,
                fan_speed: FanSpeed(80),
                swing_mode: SwingMode::Both,
                eco: true,
                turbo: false,
                sleep: true,
                fahrenheit: false,
                purifier: false,
                aux_heat: false,
                independent_aux_heat: false,
                follow_me: false,
                freeze_protection: true,
                target_humidity: 55,
                beep: true,
            };
            let framed = command::set_state(7, &sp);
            let payload = frame::parse(&framed, DeviceType::AirConditioner).unwrap();
            let c = decode_command(payload).unwrap();
            assert!(c.power && c.beep && c.eco && c.sleep && c.freeze_protection && !c.turbo);
            assert_eq!(c.mode, Some(Mode::Cool));
            assert_eq!(c.target, target as f64, "target {target}");
            assert_eq!(c.fan, 80);
            assert_eq!(c.swing, Some(SwingMode::Both));
            assert_eq!(c.humidity, 55);
        }
    }

    fn caps() -> CapabilitySet {
        CapabilitySet {
            modes: vec![Mode::Auto, Mode::Cool, Mode::Dry, Mode::Heat, Mode::FanOnly],
            swing_vertical: true,
            swing_horizontal: false,
            fan: vec![FanStep::Low, FanStep::Medium, FanStep::High, FanStep::Auto],
            fan_custom: false,
            eco: true,
            turbo_cool: true,
            turbo_heat: false,
            freeze_protection: false,
            display_control: true,
            humidity: false,
            buzzer: true,
            fahrenheit: true,
            cool_range: (16.0, 30.0),
            auto_range: (17.0, 30.0),
            heat_range: (16.0, 29.5),
            half_degrees: true,
            two_pages: false,
            extra_records: vec![(0x0040, vec![1])],
        }
    }

    fn parsed(c: &CapabilitySet) -> Capabilities {
        let pages = capability_pages(c).unwrap();
        let mut got = Capabilities::parse(&pages[0]);
        for p in &pages[1..] {
            got.merge(&Capabilities::parse(p));
        }
        got
    }

    #[test]
    fn capabilities_read_back_as_the_blueprint_says() {
        for two_pages in [false, true] {
            let mut c = caps();
            c.two_pages = two_pages;
            let got = parsed(&c);
            assert_eq!(
                got.operational_modes(),
                ["FAN_ONLY", "DRY", "COOL", "HEAT", "AUTO"]
            );
            assert_eq!(got.swing_modes(), ["OFF", "VERTICAL"]);
            assert_eq!(got.fan_speeds(), ["LOW", "MEDIUM", "HIGH", "AUTO"]);
            assert!(got.eco && got.turbo_cool && !got.turbo_heat && got.display_control);
            assert!(got.fahrenheit && got.buzzer && !got.freeze_protection);
            assert_eq!(got.min_temperature(), 16.0);
            assert_eq!(got.max_temperature(), 30.0);
            assert_eq!(got.heat_max_temperature, Some(29.5));
            assert!(got.decimals);
            assert!(
                got.unknown_ids.is_empty(),
                "0x0040 is known to breeze-proto"
            );
            assert!(!got.additional, "the last page says there is nothing more");
        }
    }

    #[test]
    fn every_expressible_mode_and_fan_set_reads_back_exactly() {
        for (_, set) in MODE_VALUES {
            let mut c = caps();
            c.modes = [
                (set[0], Mode::Heat),
                (set[1], Mode::Cool),
                (set[2], Mode::Dry),
                (set[3], Mode::Auto),
            ]
            .iter()
            .filter(|(b, _)| *b)
            .map(|(_, m)| *m)
            .chain([Mode::FanOnly])
            .collect();
            let got = parsed(&c);
            assert_eq!(
                [got.heat_mode, got.cool_mode, got.dry_mode, got.auto_mode],
                *set
            );
            assert!(!got.aux_mode && !got.aux_heat_mode);
        }
        for (_, set) in FAN_VALUES {
            let mut c = caps();
            let steps = [
                FanStep::Silent,
                FanStep::Low,
                FanStep::Medium,
                FanStep::High,
                FanStep::Auto,
            ];
            c.fan = steps
                .iter()
                .zip(set)
                .filter(|(_, b)| **b)
                .map(|(s, _)| *s)
                .collect();
            let got = parsed(&c);
            assert_eq!(
                [
                    got.supports_fan_silent(),
                    got.supports_fan_low(),
                    got.supports_fan_medium(),
                    got.supports_fan_high(),
                    got.supports_fan_auto()
                ],
                *set
            );
        }
        for (v, h) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut c = caps();
            c.swing_vertical = v;
            c.swing_horizontal = h;
            let got = parsed(&c);
            assert_eq!((got.swing_vertical, got.swing_horizontal), (v, h));
        }
    }

    #[test]
    fn inexpressible_sets_are_refused_with_the_options() {
        let mut c = caps();
        c.modes = vec![Mode::Dry, Mode::FanOnly];
        let e = capability_pages(&c).unwrap_err();
        assert!(
            e.contains("\"dry\"") && e.contains("\"heat cool dry auto fan-only\""),
            "{e}"
        );
        let mut c = caps();
        c.fan = vec![FanStep::Medium];
        assert!(capability_pages(&c)
            .unwrap_err()
            .contains("fan speeds \"medium\""));
        let mut c = caps();
        c.modes = vec![Mode::Cool];
        assert!(capability_pages(&c)
            .unwrap_err()
            .contains("must include fan-only"));
        let mut c = caps();
        c.extra_records = vec![(0x0214, vec![1])];
        assert!(capability_pages(&c).unwrap_err().contains("generates"));
    }
}
