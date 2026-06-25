//! Client-side clock synchronisation.
//!
//! Prediction, interpolation, and lag compensation all need the client to agree
//! with the server on "what tick is it now". [`ClockSync`] estimates the server's
//! clock from two signals:
//!
//! - the `server_time_ms` stamped on every [`arena_protocol::snapshot::Snapshot`]
//!   (a one-way sample of the server clock at send time), and
//! - round-trip time measured from `Ping`/`Pong` ([`ClockSync::on_pong`]).
//!
//! From these it maintains a smoothed estimate of the offset between the local and
//! server clocks, and converts a local timestamp into an estimated server
//! [`Tick`]. The server clock is treated as a millisecond counter that advances in
//! lock-step with the tick rate, so `tick == server_ms * TICK_HZ / 1000` — the
//! same mapping the authority uses when it stamps snapshots.

use arena_protocol::{TICK_HZ, Tick};

/// Default smoothing factor for the clock offset. Low enough to reject jitter,
/// high enough to track genuine drift within a second or two.
const OFFSET_ALPHA: f64 = 0.05;

/// Smoothing factor for the RTT estimate. RTT is noisier and we react a touch
/// faster so interpolation delay tracks real conditions.
const RTT_ALPHA: f64 = 0.10;

/// An exponential moving average. `None` until the first sample, after which it
/// blends each new sample with weight `alpha`.
#[derive(Debug, Clone, Copy)]
pub struct Ema {
    alpha: f64,
    value: Option<f64>,
}

impl Ema {
    pub fn new(alpha: f64) -> Self {
        Self { alpha, value: None }
    }

    /// Fold in a sample and return the updated estimate.
    pub fn update(&mut self, sample: f64) -> f64 {
        let next = match self.value {
            Some(v) => v + self.alpha * (sample - v),
            None => sample, // first sample seeds the average exactly
        };
        self.value = Some(next);
        next
    }

    pub fn get(&self) -> Option<f64> {
        self.value
    }
}

/// Estimates the server clock (and thus the server tick) on the client.
#[derive(Debug, Clone)]
pub struct ClockSync {
    /// EMA of `server_clock_now - local_clock_now`, in milliseconds. Adding this to
    /// a local timestamp yields the estimated server clock at that instant.
    offset_ms: Ema,
    /// EMA of round-trip time in milliseconds, from `Ping`/`Pong`.
    rtt_ms: Ema,
}

impl Default for ClockSync {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockSync {
    pub fn new() -> Self {
        Self {
            offset_ms: Ema::new(OFFSET_ALPHA),
            rtt_ms: Ema::new(RTT_ALPHA),
        }
    }

    /// Feed a fresh RTT measurement: `sent_ms` is the local time we put in a
    /// `Ping`, echoed back in the matching `Pong`; `now_ms` is the local time we
    /// received the `Pong`.
    pub fn on_pong(&mut self, sent_ms: u64, now_ms: u64) {
        let rtt = now_ms.saturating_sub(sent_ms) as f64;
        self.rtt_ms.update(rtt);
    }

    /// Feed a snapshot's `server_time_ms`, received locally at `now_ms`. The
    /// snapshot left the server ~half an RTT ago, so by the time it reaches us the
    /// server clock has advanced past the stamped value; we compensate with half
    /// the current RTT estimate before updating the offset.
    pub fn on_snapshot(&mut self, server_time_ms: u64, now_ms: u64) {
        let half_rtt = self.rtt_ms.get().unwrap_or(0.0) * 0.5;
        // Estimated server clock *now* = stamp + transit time.
        let server_now = server_time_ms as f64 + half_rtt;
        let sample_offset = server_now - now_ms as f64;
        self.offset_ms.update(sample_offset);
    }

    /// The smoothed round-trip time in milliseconds (0 until the first `Pong`).
    pub fn rtt_ms(&self) -> f32 {
        self.rtt_ms.get().unwrap_or(0.0) as f32
    }

    /// The estimated server clock, in milliseconds, at local time `now_ms`.
    pub fn estimated_server_ms(&self, now_ms: u64) -> f64 {
        now_ms as f64 + self.offset_ms.get().unwrap_or(0.0)
    }

    /// The estimated current server [`Tick`] at local time `now_ms`. Saturates at
    /// 0 so an early estimate (before any snapshot) can never underflow.
    pub fn estimated_server_tick(&self, now_ms: u64) -> Tick {
        let ms = self.estimated_server_ms(now_ms).max(0.0);
        ((ms * TICK_HZ as f64) / 1000.0) as Tick
    }

    /// Fractional server tick at `now_ms` — used by interpolation, which needs
    /// sub-tick precision to lerp smoothly between snapshots.
    pub fn estimated_server_tick_f(&self, now_ms: u64) -> f32 {
        let ms = self.estimated_server_ms(now_ms).max(0.0);
        ((ms * TICK_HZ as f64) / 1000.0) as f32
    }

    /// Whether we have enough information to trust the tick estimate (at least one
    /// snapshot has been seen). The renderer can hold interpolation until true.
    pub fn is_synced(&self) -> bool {
        self.offset_ms.get().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_seeds_then_blends() {
        let mut e = Ema::new(0.5);
        assert_eq!(e.update(10.0), 10.0); // first sample seeds exactly
        assert_eq!(e.update(20.0), 15.0); // halfway toward the new sample
    }

    #[test]
    fn offset_maps_local_time_to_server_tick() {
        let mut clock = ClockSync::new();
        // No RTT seen yet → half_rtt = 0. Server clock equals the stamp at receive.
        // Pretend the server is 5000 ms ahead of our local clock.
        clock.on_snapshot(15_000, 10_000);
        assert!(clock.is_synced());
        // At local time 10_000 the server clock is ~15_000 ms → tick 15000*64/1000.
        let tick = clock.estimated_server_tick(10_000);
        assert_eq!(tick, (15_000u64 * TICK_HZ as u64 / 1000) as Tick);
    }

    #[test]
    fn rtt_tracks_pongs() {
        let mut clock = ClockSync::new();
        clock.on_pong(1000, 1040); // 40 ms round trip
        assert!((clock.rtt_ms() - 40.0).abs() < 0.001);
    }
}
