//! Bandwidth adaptation for three desktop streaming quality preferences.
//!
//! Smooth preserves 60 FPS, Sharp preserves pixels, and Balanced first gives up
//! 60 -> 30 FPS at native resolution. All decisions use the same bandwidth
//! estimate, hysteresis and settling period. Tests simulate changing bandwidth
//! without relying on noisy wall-clock throughput benchmarks.

use std::time::{Duration, Instant};

use conductor_proto::QualityMode;

/// Estimated bits per output pixel and frame for desktop video. This is a
/// policy heuristic, not an encoder measurement; WebRTC controls the bitrate.
const BITS_PER_PIXEL: f64 = 0.03;
const SMOOTH: &[(f64, u32)] = &[(1.0, 60), (0.75, 60), (0.5, 60), (0.375, 60)];
const BALANCED: &[(f64, u32)] = &[
    (1.0, 60),
    (1.0, 30),
    (0.75, 30),
    (0.5, 30),
    (0.375, 30),
];
const SHARP: &[(f64, u32)] = &[(1.0, 60), (1.0, 30), (1.0, 20)];
const DOWN_AFTER: Duration = Duration::from_secs(3);
const UP_AFTER: Duration = Duration::from_secs(10);
const UP_MARGIN: f64 = 1.25;
const SETTLE: Duration = Duration::from_secs(10);

pub struct Quality {
    mode: QualityMode,
    step: usize,
    pending: Option<(bool, Instant)>,
    started: Instant,
}

impl Quality {
    pub fn new(mode: QualityMode, now: Instant) -> Self {
        Self { mode, step: 0, pending: None, started: now }
    }

    fn tiers(&self) -> &'static [(f64, u32)] {
        match self.mode {
            QualityMode::Smooth => SMOOTH,
            QualityMode::Balanced => BALANCED,
            QualityMode::Sharp => SHARP,
        }
    }

    pub fn scale(&self) -> f64 {
        self.tiers()[self.step].0
    }

    pub fn fps(&self) -> u32 {
        self.tiers()[self.step].1
    }

    fn need(&self, step: usize, pixels: u64) -> f64 {
        let (scale, fps) = self.tiers()[step];
        pixels as f64 * scale * scale * fps as f64 * BITS_PER_PIXEL
    }

    /// Choose the highest tier supported by the estimated network bandwidth.
    fn carried(&self, bitrate: u32, pixels: u64, margin: f64) -> usize {
        (0..self.tiers().len())
            .find(|&step| self.need(step, pixels) * margin <= bitrate as f64)
            .unwrap_or(self.tiers().len() - 1)
    }

    /// Select a preference, applying its bitrate tier without a settling delay.
    /// Returns whether the encoder's resolution or frame rate must change.
    pub fn set_mode(&mut self, mode: QualityMode, bitrate: u32, pixels: u64) -> bool {
        if mode == self.mode {
            return false;
        }
        let before = (self.scale(), self.fps());
        self.mode = mode;
        self.step = if pixels == 0 { 0 } else { self.carried(bitrate, pixels, 1.0) };
        self.pending = None;
        before != (self.scale(), self.fps())
    }

    /// Hysteresis prevents transient drops from repeatedly restarting encoding.
    pub fn update(&mut self, bitrate: u32, pixels: u64, now: Instant) -> bool {
        if pixels == 0 || now.duration_since(self.started) < SETTLE {
            return false;
        }
        let down = self.carried(bitrate, pixels, 1.0);
        let up = self.carried(bitrate, pixels, UP_MARGIN);
        let target = if down > self.step {
            down
        } else if up < self.step {
            up
        } else {
            self.pending = None;
            return false;
        };
        let lower = target > self.step;
        match self.pending {
            Some((l, since)) if l == lower => {
                if now.duration_since(since) < if lower { DOWN_AFTER } else { UP_AFTER } {
                    return false;
                }
                self.step = target;
                self.pending = None;
                true
            }
            _ => {
                self.pending = Some((lower, now));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1440: u64 = 2560 * 1440;

    fn start(mode: QualityMode) -> (Quality, Instant) {
        let now = Instant::now();
        (Quality::new(mode, now), now + SETTLE)
    }

    fn settle(q: &mut Quality, bitrate: u32, t: Instant) {
        assert!(!q.update(bitrate, P1440, t));
        assert!(q.update(bitrate, P1440, t + DOWN_AFTER));
    }

    #[test]
    fn full_rate_on_fast_connection() {
        for mode in [QualityMode::Smooth, QualityMode::Balanced, QualityMode::Sharp] {
            let (mut q, t) = start(mode);
            assert!(!q.update(20_000_000, P1440, t));
            assert_eq!((q.scale(), q.fps()), (1.0, 60));
        }
    }

    #[test]
    fn balanced_keeps_native_at_thirty_before_scaling() {
        let (mut q, t) = start(QualityMode::Balanced);
        settle(&mut q, 5_000_000, t);
        assert_eq!((q.scale(), q.fps()), (1.0, 30));

        settle(&mut q, 2_000_000, t + DOWN_AFTER + Duration::from_secs(1));
        assert_eq!((q.scale(), q.fps()), (0.75, 30));
    }

    #[test]
    fn balanced_recovers_resolution_before_framerate() {
        let (mut q, t) = start(QualityMode::Balanced);
        settle(&mut q, 2_000_000, t);
        assert_eq!((q.scale(), q.fps()), (0.75, 30));
        let t = t + DOWN_AFTER + Duration::from_secs(1);
        assert!(!q.update(5_000_000, P1440, t));
        assert!(q.update(5_000_000, P1440, t + UP_AFTER));
        assert_eq!((q.scale(), q.fps()), (1.0, 30));
        let t = t + UP_AFTER + Duration::from_secs(1);
        assert!(!q.update(10_000_000, P1440, t));
        assert!(q.update(10_000_000, P1440, t + UP_AFTER));
        assert_eq!((q.scale(), q.fps()), (1.0, 60));
    }

    #[test]
    fn legacy_smooth_and_sharp_preserve_their_tradeoff() {
        let (mut smooth, t) = start(QualityMode::Smooth);
        settle(&mut smooth, 3_000_000, t);
        assert_eq!((smooth.scale(), smooth.fps()), (0.5, 60));
        let (mut sharp, t) = start(QualityMode::Sharp);
        settle(&mut sharp, 3_000_000, t);
        assert_eq!((sharp.scale(), sharp.fps()), (1.0, 20));
    }

    #[test]
    fn brief_bandwidth_dips_do_not_trigger_restarts() {
        let (mut q, t) = start(QualityMode::Balanced);
        for i in 0..200 {
            let rate = if i % 4 == 0 { 1_000_000 } else { 20_000_000 };
            assert!(!q.update(rate, P1440, t + Duration::from_millis(i * 50)));
        }
        assert_eq!((q.scale(), q.fps()), (1.0, 60));
    }

    #[test]
    fn steady_bandwidth_change_is_bounded() {
        let (mut q, t) = start(QualityMode::Balanced);
        let mut changes = 0;
        for i in 0..200 {
            if q.update(5_000_000, P1440, t + Duration::from_millis(i * 50)) {
                changes += 1;
            }
        }
        assert_eq!(changes, 1);
        assert_eq!((q.scale(), q.fps()), (1.0, 30));
    }

    #[test]
    fn changing_mode_reselects_tier_immediately() {
        let (mut q, _) = start(QualityMode::Balanced);
        assert!(q.set_mode(QualityMode::Smooth, 5_000_000, P1440));
        assert_eq!((q.scale(), q.fps()), (0.75, 60));
        assert!(q.set_mode(QualityMode::Balanced, 5_000_000, P1440));
        assert_eq!((q.scale(), q.fps()), (1.0, 30));
        assert!(!q.set_mode(QualityMode::Balanced, 5_000_000, P1440));
    }

    #[test]
    fn bitrate_sweep_keeps_priority_order() {
        for bitrate_mbps in 1..=40 {
            let (mut q, t) = start(QualityMode::Balanced);
            let bitrate = bitrate_mbps * 1_000_000;
            _ = q.update(bitrate, P1440, t);
            _ = q.update(bitrate, P1440, t + DOWN_AFTER);
            assert!(q.fps() >= 30);
            if q.fps() == 60 {
                assert_eq!(q.scale(), 1.0);
            }
            if bitrate >= 4_000_000 {
                assert_eq!(q.scale(), 1.0);
            }
        }
    }
}
