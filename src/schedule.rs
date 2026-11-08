// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::num::NonZeroU64;

const NANOSECONDS_PER_SECOND: u128 = 1_000_000_000;

/// The absolute arrival schedule of the open-loop mode: request `k` is due `floor(k × 10^9 / rate)` nanoseconds after the start.
///
/// Every offset is computed from its ordinal, never accumulated from the previous one or reset to an actual send or completion time, so rounding never drifts and a late response never moves the requests planned after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    rate: NonZeroU64,
}

impl Schedule {
    /// A schedule of `rate` requests per second.
    pub fn new(rate: NonZeroU64) -> Self {
        Self { rate }
    }

    /// The offset of request `ordinal` from the start, in nanoseconds.
    pub fn offset(self, ordinal: u64) -> u64 {
        let offset = u128::from(ordinal) * NANOSECONDS_PER_SECOND / u128::from(self.rate.get());
        u64::try_from(offset).unwrap_or(u64::MAX)
    }

    /// The first ordinal due at or after `offset` nanoseconds: `ceil(offset × rate / 10^9)`, the exact inverse of [`Schedule::offset`]'s rounding. The number of requests planned in `[a, b)` is `first_at_or_after(b) - first_at_or_after(a)`.
    pub fn first_at_or_after(self, offset: u64) -> u64 {
        let ordinal =
            (u128::from(offset) * u128::from(self.rate.get())).div_ceil(NANOSECONDS_PER_SECOND);
        u64::try_from(ordinal).unwrap_or(u64::MAX)
    }
}

/// Walks one worker's ordinals in increasing order.
///
/// With `N` connections, connection `g` sends ordinals `g, g + N, g + 2N, …`, so every connection has its own evenly spaced schedule. Worker `w` of `W` owns connections `w, w + W, w + 2W, …`, so its ordinals in increasing order are `w + mW + jN` for each round `j` and each of its connections `m`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    worker: u64,
    workers: u64,
    connections: u64,
    local: u64,
    local_count: u64,
    round: u64,
}

impl Cursor {
    /// The cursor at worker `worker`'s first ordinal, for `workers` workers sharing `connections` connections round-robin. Every worker owns at least one connection.
    pub fn new(worker: usize, workers: usize, connections: usize) -> Self {
        let (worker, workers, connections) = (worker as u64, workers as u64, connections as u64);
        Self {
            worker,
            workers,
            connections,
            local: 0,
            local_count: (connections - worker).div_ceil(workers),
            round: 0,
        }
    }

    pub fn ordinal(&self) -> u64 {
        self.round * self.connections + self.worker + self.local * self.workers
    }

    /// The worker's own index of the connection that sends [`Cursor::ordinal`].
    pub fn local(&self) -> usize {
        self.local as usize
    }

    pub fn advance(&mut self) {
        self.local += 1;
        if self.local == self.local_count {
            self.local = 0;
            self.round += 1;
        }
    }
}

/// How many of `first, first + step, first + 2 × step, …` are below `limit`.
pub fn count_below(first: u64, step: u64, limit: u64) -> u64 {
    limit.saturating_sub(first).div_ceil(step)
}

#[cfg(test)]
mod tests;
