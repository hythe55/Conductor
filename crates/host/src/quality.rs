//! What gives way when the connection can't carry the whole picture: its size
//! (smooth, the default) or how many frames it sends (sharp). The client
//! chooses; on a fast enough connection both send everything.

use std::time::{Duration, Instant};

/// Bits each pixel of each frame needs to look clean. Low, because most of a
/// desktop stays still from one frame to the next.
const BITS_PER_PIXEL: f64 = 0.03;
/// Smooth: the picture's width and height, as parts of the full size.
const SCALES: [f64; 4] = [1.0, 0.75, 0.5, 0.375];
/// Sharp: frames each second.
const RATES: [u32; 3] = [60, 30, 20];
/// How long the connection stays too slow before giving way.
const DOWN_AFTER: Duration = Duration::from_secs(3);
/// How long it stays fast enough, with [`UP_MARGIN`] to spare, before taking it back.
const UP_AFTER: Duration = Duration::from_secs(10);
const UP_MARGIN: f64 = 1.25;
/// The bitrate estimate starts as a guess; it's left to settle this long.
const SETTLE: Duration = Duration::from_secs(10);

pub struct Quality {
    sharp: bool,
    /// Into [`SCALES`] or [`RATES`].
    step: usize,
    /// Since when a step down (true) or up (false) has suited the bitrate.
    pending: Option<(bool, Instant)>,
    started: Instant,
}

impl Quality {
    pub fn new(sharp: bool, now: Instant) -> Self {
        Self { sharp, step: 0, pending: None, started: now }
    }

    pub fn scale(&self) -> f64 {
        if self.sharp { 1.0 } else { SCALES[self.step] }
    }

    pub fn fps(&self) -> u32 {
        if self.sharp { RATES[self.step] } else { RATES[0] }
    }

    fn steps(&self) -> usize {
        if self.sharp { RATES.len() } else { SCALES.len() }
    }

    /// Bits per second step `step` needs for a picture of `pixels` at full size.
    fn need(&self, step: usize, pixels: u64) -> f64 {
        let (scale, fps) = if self.sharp { (1.0, RATES[step]) } else { (SCALES[step], RATES[0]) };
        pixels as f64 * scale * scale * fps as f64 * BITS_PER_PIXEL
    }

    /// The first step `bitrate` carries, or the last when it carries none.
    fn carried(&self, bitrate: u32, pixels: u64, margin: f64) -> usize {
        (0..self.steps()).find(|&s| self.need(s, pixels) * margin <= bitrate as f64).unwrap_or(self.steps() - 1)
    }

    /// Switches between smooth and sharp, at once to the step `bitrate` carries.
    /// True when the picture changes.
    pub fn set_sharp(&mut self, sharp: bool, bitrate: u32, pixels: u64) -> bool {
        if sharp == self.sharp {
            return false;
        }
        let before = (self.scale(), self.fps());
        self.sharp = sharp;
        self.step = if pixels == 0 { 0 } else { self.carried(bitrate, pixels, 1.0) };
        self.pending = None;
        before != (self.scale(), self.fps())
    }

    /// Follows the bitrate the connection carries, for a picture of `pixels` at
    /// full size. True when the picture changes.
    pub fn update(&mut self, bitrate: u32, pixels: u64, now: Instant) -> bool {
        if pixels == 0 || now.duration_since(self.started) < SETTLE {
            return false;
        }
        let down = self.carried(bitrate, pixels, 1.0);
        // Going back up needs room to spare, so a bitrate near the edge doesn't flip it back and forth.
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

    fn settled() -> (Quality, Instant) {
        let t = Instant::now();
        (Quality::new(false, t), t + SETTLE)
    }

    #[test]
    fn fast_connection_sends_everything() {
        let (mut q, t) = settled();
        assert!(!q.update(20_000_000, P1440, t));
        assert!(!q.update(20_000_000, P1440, t + DOWN_AFTER * 2));
        assert_eq!((q.scale(), q.fps()), (1.0, 60));
    }

    #[test]
    fn smooth_gets_smaller_then_back() {
        let (mut q, t) = settled();
        // 1440p at 60 needs about 6.6 Mbps.
        assert!(!q.update(3_000_000, P1440, t));
        assert!(!q.update(3_000_000, P1440, t + DOWN_AFTER / 2));
        assert!(q.update(3_000_000, P1440, t + DOWN_AFTER));
        assert_eq!((q.scale(), q.fps()), (0.5, 60));
        // Enough for full size, but with room to spare only at three quarters.
        let t = t + DOWN_AFTER;
        assert!(!q.update(7_000_000, P1440, t));
        assert!(q.update(7_000_000, P1440, t + UP_AFTER));
        assert_eq!(q.scale(), 0.75);
        assert!(!q.update(9_000_000, P1440, t + UP_AFTER * 2));
        assert!(q.update(9_000_000, P1440, t + UP_AFTER * 3));
        assert_eq!(q.scale(), 1.0);
    }

    #[test]
    fn sharp_drops_frames_instead() {
        let t = Instant::now();
        let mut q = Quality::new(true, t);
        let t = t + SETTLE;
        assert!(!q.update(3_000_000, P1440, t));
        assert!(q.update(3_000_000, P1440, t + DOWN_AFTER));
        assert_eq!((q.scale(), q.fps()), (1.0, 20));
    }

    #[test]
    fn waits_for_the_estimate_to_settle() {
        let t = Instant::now();
        let mut q = Quality::new(false, t);
        assert!(!q.update(1_000_000, P1440, t));
        assert!(!q.update(1_000_000, P1440, t + DOWN_AFTER));
        assert_eq!(q.scale(), 1.0);
    }

    #[test]
    fn a_brief_dip_changes_nothing() {
        let (mut q, t) = settled();
        assert!(!q.update(3_000_000, P1440, t));
        assert!(!q.update(20_000_000, P1440, t + DOWN_AFTER / 2));
        assert!(!q.update(3_000_000, P1440, t + DOWN_AFTER));
        assert_eq!(q.scale(), 1.0);
    }

    #[test]
    fn switching_applies_at_once() {
        let (mut q, _) = settled();
        // Nothing to give up on a fast connection.
        assert!(!q.set_sharp(true, 20_000_000, P1440));
        assert!(!q.set_sharp(false, 20_000_000, P1440));
        assert!(q.set_sharp(true, 5_000_000, P1440));
        assert_eq!((q.scale(), q.fps()), (1.0, 30));
        assert!(q.set_sharp(false, 5_000_000, P1440));
        assert_eq!((q.scale(), q.fps()), (0.75, 60));
    }
}
