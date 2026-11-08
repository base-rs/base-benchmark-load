// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::time::Instant;

/// A monotonic time source in nanoseconds from an arbitrary origin. Workers read time only through it, so tests can control time.
pub trait Clock: Sync {
    fn now(&self) -> u64;
}

/// The monotonic clock (`CLOCK_MONOTONIC` on Linux), counted from when it was created.
#[derive(Clone, Copy, Debug)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// A clock that advances by a fixed step every time it is read, for deterministic tests of loops that would otherwise wait for real time.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct StepClock {
    now: std::sync::atomic::AtomicU64,
    step: u64,
}

#[cfg(test)]
impl StepClock {
    pub fn new(start: u64, step: u64) -> Self {
        Self {
            now: start.into(),
            step,
        }
    }

    pub fn set(&self, now: u64) {
        self.now.store(now, std::sync::atomic::Ordering::Relaxed);
    }

    /// Move the clock `by` ahead, as if that long passed between two reads.
    pub fn advance(&self, by: u64) {
        self.now.fetch_add(by, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
impl Clock for StepClock {
    fn now(&self) -> u64 {
        self.now
            .fetch_add(self.step, std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monotonic_clock_counts_from_its_origin() {
        let clock = MonotonicClock::new();
        let first = clock.now();
        let elapsed = clock.origin.elapsed();
        assert!(u128::from(first) <= elapsed.as_nanos());
        assert!(clock.now() >= first);
    }

    #[test]
    fn step_clocks_advance_on_every_read() {
        let clock = StepClock::new(10, 5);
        assert_eq!(clock.now(), 10);
        assert_eq!(clock.now(), 15);
        clock.set(100);
        assert_eq!(clock.now(), 100);
        clock.advance(1000);
        assert_eq!(clock.now(), 1105);
    }
}
