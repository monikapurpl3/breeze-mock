//! Weather and rooms: just enough physics for a room to behave like a room.
//!
//! One lumped heat capacity per room, losing heat to outdoors in proportion to
//! the difference, gaining it from people, appliances, the sun and whatever
//! the air conditioners put in or take out:
//!
//! ```text
//! C · dT/dt = P_units + P_gains + P_sun + UA · (T_outdoor − T)
//! ```
//!
//! Integrated in steps of at most [`MAX_STEP`] simulated seconds, so a
//! blueprint running an hour a second stays stable instead of overshooting.

use crate::blueprint::{Room, Weather};

/// The longest single integration step, in simulated seconds.
pub const MAX_STEP: f64 = 20.0;

/// Outdoor temperature at a time of day (hours, 0 to 24).
pub fn outdoor(weather: &Weather, hour: f64) -> f64 {
    let hour = hour.rem_euclid(24.0);
    match weather {
        Weather::Fixed(t) => *t,
        Weather::Daily {
            mean,
            swing,
            warmest_hour,
        } => mean + swing / 2.0 * (std::f64::consts::TAU * (hour - warmest_hour) / 24.0).cos(),
        Weather::Points(points) => {
            // Linear between points, wrapping from the last back to the first
            // across midnight.
            let n = points.len();
            for i in 0..n {
                let (h0, t0) = points[i];
                let (h1, t1) = if i + 1 < n {
                    points[i + 1]
                } else {
                    (points[0].0 + 24.0, points[0].1)
                };
                let h = if hour < points[0].0 {
                    hour + 24.0
                } else {
                    hour
                };
                if (h0..=h1).contains(&h) {
                    let f = if h1 > h0 { (h - h0) / (h1 - h0) } else { 0.0 };
                    return t0 + (t1 - t0) * f;
                }
            }
            points[0].1
        }
    }
}

/// Solar gain: a half-sine from 06:00 to 18:00, peaking at noon.
pub fn sun_factor(hour: f64) -> f64 {
    let h = hour.rem_euclid(24.0);
    if (6.0..18.0).contains(&h) {
        (std::f64::consts::PI * (h - 6.0) / 12.0).sin()
    } else {
        0.0
    }
}

/// One room's temperature, advanced by `dt` simulated seconds with `units_w`
/// watts from its air conditioners (negative when cooling).
pub fn step(
    room: &Room,
    temperature: f64,
    outdoor_c: f64,
    hour: f64,
    units_w: f64,
    dt: f64,
) -> f64 {
    let gains = room.internal_gain_w + room.sun_gain_w * sun_factor(hour);
    let flow = units_w + gains + room.loss_w_per_k * (outdoor_c - temperature);
    temperature + flow * dt / room.capacity_j_per_k
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> Room {
        Room {
            origin: crate::blueprint::Origin {
                file: "t.xml".into(),
                line: 1,
                col: 1,
                element: "room".into(),
            },
            id: "r".into(),
            name: "R".into(),
            area_m2: 14.0,
            height_m: 2.6,
            loss_w_per_k: 49.0,
            capacity_j_per_k: 14.0 * 2.6 * 1.2 * 1005.0 * 8.0,
            internal_gain_w: 0.0,
            sun_gain_w: 0.0,
            start_c: 20.0,
        }
    }

    #[test]
    fn the_daily_cycle_peaks_when_it_says() {
        let w = Weather::Daily {
            mean: 6.0,
            swing: 8.0,
            warmest_hour: 15.0,
        };
        assert!((outdoor(&w, 15.0) - 10.0).abs() < 1e-9);
        assert!((outdoor(&w, 3.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn hourly_points_interpolate_and_wrap_midnight() {
        let w = Weather::Points(vec![(6.0, 0.0), (18.0, 12.0)]);
        assert!((outdoor(&w, 12.0) - 6.0).abs() < 1e-9);
        // 18:00 -> 06:00 falls from 12 to 0 over twelve hours.
        assert!((outdoor(&w, 0.0) - 6.0).abs() < 1e-9);
        assert!((outdoor(&w, 3.0) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn an_unheated_room_drifts_towards_outdoors_and_a_heated_one_warms() {
        let r = room();
        let mut cold = 20.0;
        let mut warm = 20.0;
        for _ in 0..(3600.0 / MAX_STEP) as usize {
            cold = step(&r, cold, 0.0, 0.0, 0.0, MAX_STEP);
            warm = step(&r, warm, 0.0, 0.0, 3000.0, MAX_STEP);
        }
        assert!(cold < 20.0 && cold > 0.0, "an hour at 0 outside: {cold}");
        assert!(warm > 20.0, "3 kW for an hour: {warm}");
    }
}
