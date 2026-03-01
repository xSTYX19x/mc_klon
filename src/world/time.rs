use std::f32::consts::{PI, TAU};

pub(crate) const TICKS_PER_DAY: f32 = 24_000.0;
pub(crate) const MC_TICKS_PER_SECOND: f32 = 20.0;
const MIN_SUN_BRIGHTNESS: f32 = 0.2;

#[derive(Clone, Copy, Debug)]
pub(crate) struct WorldTime {
    pub(crate) ticks_total: f64,
    pub(crate) ticks: f32,
    pub(crate) day: i64,
    pub(crate) phase: u8,
    pub(crate) celestial_angle: f32,
    pub(crate) sun_brightness: f32,
    pub(crate) sky_brightness: f32,
}

impl Default for WorldTime {
    fn default() -> Self {
        Self::from_total_ticks(6_000.0)
    }
}

impl WorldTime {
    pub(crate) fn from_total_ticks(ticks_total: f64) -> Self {
        let mut time = Self {
            ticks_total: ticks_total.max(0.0),
            ticks: 0.0,
            day: 0,
            phase: 0,
            celestial_angle: 0.0,
            sun_brightness: 1.0,
            sky_brightness: 1.0,
        };
        time.recompute(0.0);
        time
    }

    pub(crate) fn advance(&mut self, dt_seconds: f32) {
        if !dt_seconds.is_finite() {
            return;
        }
        self.ticks_total += (dt_seconds.max(0.0) * MC_TICKS_PER_SECOND) as f64;
        self.recompute(0.0);
    }

    pub(crate) fn recompute(&mut self, partial: f32) {
        self.ticks = (self.ticks_total.rem_euclid(TICKS_PER_DAY as f64)) as f32;
        self.day = (self.ticks_total / TICKS_PER_DAY as f64).floor() as i64;
        self.phase = self.day.rem_euclid(8) as u8;
        self.celestial_angle = calculate_celestial_angle(self.ticks, partial);
        self.sun_brightness = calculate_sun_brightness(self.ticks, partial);
        self.sky_brightness = sky_brightness_from_sun(self.sun_brightness);
    }
}

pub(crate) fn calculate_celestial_angle(world_time_ticks: f32, partial: f32) -> f32 {
    let i = world_time_ticks.rem_euclid(TICKS_PER_DAY);
    let mut f = ((i + partial) / TICKS_PER_DAY) - 0.25;
    f = f.fract();
    if f < 0.0 {
        f += 1.0;
    }
    let f1 = 0.5 - (f * PI).cos() * 0.5;
    ((f * 2.0) + f1) / 3.0
}

pub(crate) fn calculate_sun_brightness(world_time_ticks: f32, partial: f32) -> f32 {
    sun_brightness_from_angle(calculate_celestial_angle(world_time_ticks, partial))
}

pub(crate) fn sun_brightness_from_angle(celestial_angle: f32) -> f32 {
    let mut f = 1.0 - ((celestial_angle * TAU).cos() * 2.0 + 0.2);
    f = f.clamp(0.0, 1.0);
    f = 1.0 - f;
    f * 0.8 + MIN_SUN_BRIGHTNESS
}

pub(crate) fn sky_brightness_from_sun(sun_brightness: f32) -> f32 {
    ((sun_brightness - MIN_SUN_BRIGHTNESS) / (1.0 - MIN_SUN_BRIGHTNESS)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noon_defaults_are_bright() {
        let t = WorldTime::default();
        assert!((t.ticks - 6_000.0).abs() < 0.001);
        assert_eq!(t.day, 0);
        assert_eq!(t.phase, 0);
        assert!(t.sky_brightness > 0.95);
    }

    #[test]
    fn moon_phase_advances_per_day() {
        let t = WorldTime::from_total_ticks(24_000.0 * 10.0);
        assert_eq!(t.day, 10);
        assert_eq!(t.phase, 2);
    }
}
