//! Analytic spring curve (damped harmonic oscillator), sampled into Composition keyframes.
//!
//! Composition's built-in spring animation needs `IReference<Vector2>`, which the `windows` crate
//! does not provide. By sampling the curve ourselves, the animation still runs entirely on DWM.
//! Parameters mimic SwiftUI: `response` = undamped oscillation period (seconds).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub damping: f32,
    pub response: f32,
}

impl Spring {
    /// Expanding: bounces slightly, like the Dynamic Island.
    pub const EXPAND: Spring = Spring { damping: 0.74, response: 0.42 };
    /// Shrinking: critically damped, fast with no bounce.
    pub const COLLAPSE: Spring = Spring { damping: 1.0, response: 0.32 };

    /// Relative position (0 at the start, 1 at the target) at second `t`.
    pub fn value(&self, t: f32) -> f32 {
        let w0 = std::f32::consts::TAU / self.response;
        if self.damping < 1.0 {
            let wd = w0 * (1.0 - self.damping * self.damping).sqrt();
            let decay = (-self.damping * w0 * t).exp();
            1.0 - decay * ((wd * t).cos() + (self.damping * w0 / wd) * (wd * t).sin())
        } else {
            1.0 - (-w0 * t).exp() * (1.0 + w0 * t)
        }
    }

    /// Time until the spring settles (displacement < 0.2%), in milliseconds.
    pub fn duration_ms(&self) -> u32 {
        let mut last_moving = 0;
        for ms in 0..3000 {
            if (1.0 - self.value(ms as f32 / 1000.0)).abs() > 0.002 {
                last_moving = ms;
            }
        }
        last_moving + 1
    }

    /// `steps` points (progress 0..1, value) for a linear keyframe. The last point is exactly (1, 1).
    pub fn keyframes(&self, steps: usize) -> Vec<(f32, f32)> {
        let total = self.duration_ms() as f32 / 1000.0;
        (1..=steps)
            .map(|i| {
                let p = i as f32 / steps as f32;
                (p, if i == steps { 1.0 } else { self.value(p * total) })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_zero_and_ends_at_one() {
        for s in [Spring::EXPAND, Spring::COLLAPSE] {
            assert!(s.value(0.0).abs() < 1e-6);
            assert_eq!(*s.keyframes(24).last().unwrap(), (1.0, 1.0));
        }
    }

    #[test]
    fn expand_overshoots_and_collapse_does_not() {
        let peak = |s: Spring| s.keyframes(60).iter().map(|k| k.1).fold(0.0f32, f32::max);
        assert!(peak(Spring::EXPAND) > 1.01);
        assert!(peak(Spring::COLLAPSE) <= 1.0);
    }

    #[test]
    fn durations_are_reasonable() {
        assert!((350..700).contains(&Spring::EXPAND.duration_ms()));
        assert!((250..550).contains(&Spring::COLLAPSE.duration_ms()));
    }
}
