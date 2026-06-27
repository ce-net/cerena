//! The shared clock that makes "everyone runs the server and they agree" possible.
//!
//! An input is scheduled to apply at a specific simulation `tick`, the SAME tick on
//! every replica. With roughly NTP-synced wall clocks, every node derives the same
//! current tick from wall-clock time ([`tick_at`]), so a tick-tagged input lands on
//! the same tick everywhere regardless of network jitter.

use arena_protocol::{Tick, TICK_HZ};

/// Milliseconds per simulation tick at Cerena's [`TICK_HZ`] (64 Hz) — the rate every
/// replica advances at.
pub const TICK_MS: f64 = 1000.0 / TICK_HZ as f64;

/// A fixed epoch (ms since the Unix epoch) the shared tick clock counts from.
/// Arbitrary, but it MUST be the same constant on every replica so they all derive
/// the same tick number from wall-clock time. This is the "shared clock".
pub const TICK_EPOCH_MS: f64 = 1_700_000_000_000.0;

/// Ticks of input delay: a locally-generated input applies at
/// `current_tick + INPUT_DELAY` on every replica, giving the network time to deliver
/// it everywhere before that tick is simulated. At 64 Hz, 8 ticks is ~125 ms — the
/// budget for an input to reach the other replicas in a zone. Your own body can
/// still feel instant via a local-echo layer above this; the canonical scheduled
/// input is what every replica (including yours) agrees to simulate.
pub const INPUT_DELAY: Tick = 8;

/// The canonical simulation tick for a wall-clock time (ms since the Unix epoch).
/// Saturates at 0, and saturates at [`Tick::MAX`] rather than wrapping (a session
/// never runs the ~2 years that would take at 64 Hz).
pub fn tick_at(now_ms: f64) -> Tick {
    let t = (now_ms - TICK_EPOCH_MS) / TICK_MS;
    if t < 0.0 {
        0
    } else if t >= Tick::MAX as f64 {
        Tick::MAX
    } else {
        t as Tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_at_is_monotonic_and_saturating() {
        assert_eq!(tick_at(TICK_EPOCH_MS), 0);
        assert_eq!(tick_at(TICK_EPOCH_MS - 1_000.0), 0, "before the epoch saturates to 0");
        // One second after the epoch is exactly TICK_HZ ticks.
        assert_eq!(tick_at(TICK_EPOCH_MS + 1000.0), TICK_HZ);
        assert!(tick_at(TICK_EPOCH_MS + 2000.0) > tick_at(TICK_EPOCH_MS + 1000.0));
    }
}
