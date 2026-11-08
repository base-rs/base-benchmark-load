// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use serde_json::{Value, json};

/// The precision of the reported distributions: three significant decimal digits, HdrHistogram's usual setting.
pub const DEFAULT_SIGNIFICANT_DIGITS: u8 = 3;

/// A log-linear histogram of `u64` values, such as latencies in nanoseconds, with HdrHistogram's bucketing at a unit magnitude of one.
///
/// With `d` significant digits, let `2^s` be the smallest power of two of at least `2 × 10^d`. Values below `2^s` are counted exactly. Above that, every power of two is divided into `2^(s-1)` equal buckets, so a bucket is never wider than one part in `10^d` of the values it holds. Buckets are allocated up to the largest recorded value, so a histogram of short latencies stays small.
///
/// Histograms of the same precision merge exactly: their bucket counts add. Quantiles are the highest value equivalent to the bucket that holds them, clamped to the exact minimum and maximum, as HdrHistogram reports them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Histogram {
    significant_digits: u8,
    magnitude: u32,
    counts: Vec<u64>,
    count: u64,
    min: u64,
    max: u64,
    sum: u128,
}

impl Histogram {
    /// An empty histogram with `significant_digits` of precision, from 1 to 5.
    ///
    /// # Panics
    ///
    /// If `significant_digits` is outside that range.
    pub fn new(significant_digits: u8) -> Self {
        assert!(
            (1..=5).contains(&significant_digits),
            "a histogram has from 1 to 5 significant digits"
        );
        let exact = 2 * 10u64.pow(u32::from(significant_digits));
        Self {
            significant_digits,
            magnitude: exact.next_power_of_two().trailing_zeros(),
            counts: Vec::new(),
            count: 0,
            min: u64::MAX,
            max: 0,
            sum: 0,
        }
    }

    pub fn significant_digits(&self) -> u8 {
        self.significant_digits
    }

    fn index(&self, value: u64) -> usize {
        if value < 1 << self.magnitude {
            return value as usize;
        }
        let shift = value.ilog2() - (self.magnitude - 1);
        ((shift as usize) << (self.magnitude - 1)) + (value >> shift) as usize
    }

    /// The lowest value counted in bucket `index`.
    fn lowest(&self, index: usize) -> u64 {
        let half = 1 << (self.magnitude - 1);
        if index < 2 * half {
            return index as u64;
        }
        let shift = index / half - 1;
        ((index - shift * half) as u64) << shift
    }

    /// The highest value counted in bucket `index`.
    fn highest(&self, index: usize) -> u64 {
        let half = 1 << (self.magnitude - 1);
        if index < 2 * half {
            return index as u64;
        }
        let shift = index / half - 1;
        self.lowest(index) + ((1 << shift) - 1)
    }

    pub fn record(&mut self, value: u64) {
        let index = self.index(value);
        if index >= self.counts.len() {
            self.counts.resize(index + 1, 0);
        }
        self.counts[index] += 1;
        self.count += 1;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.sum += u128::from(value);
    }

    /// Add every observation of `other`.
    ///
    /// # Panics
    ///
    /// If the histograms have different precision, which would make the merged buckets inexact.
    pub fn merge(&mut self, other: &Self) {
        assert_eq!(
            self.significant_digits, other.significant_digits,
            "only histograms of the same precision merge exactly"
        );
        if other.counts.len() > self.counts.len() {
            self.counts.resize(other.counts.len(), 0);
        }
        for (count, other) in self.counts.iter_mut().zip(&other.counts) {
            *count += other;
        }
        self.count += other.count;
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.sum += other.sum;
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn min(&self) -> Option<u64> {
        (self.count > 0).then_some(self.min)
    }

    pub fn max(&self) -> Option<u64> {
        (self.count > 0).then_some(self.max)
    }

    /// The exact mean of the recorded values.
    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum as f64 / self.count as f64)
    }

    /// The value at `quantile`, from 0 (the minimum) to 1 (the maximum): the highest value equivalent to the bucket holding the observation of rank `ceil(quantile × count)`, clamped to the recorded range.
    pub fn value_at_quantile(&self, quantile: f64) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        if quantile <= 0.0 {
            return Some(self.min);
        }
        let rank = ((quantile * self.count as f64).ceil() as u64).clamp(1, self.count);
        let mut seen = 0;
        let index = self
            .counts
            .iter()
            .position(|count| {
                seen += count;
                seen >= rank
            })
            .expect("the bucket counts sum to the total count");
        Some(self.highest(index).clamp(self.min, self.max))
    }

    /// The histogram as JSON: its precision, count, exact minimum, maximum and mean, and its non-empty buckets as `[lowest value, count]` pairs in increasing order.
    pub fn to_json(&self) -> Value {
        let buckets: Vec<Value> = self
            .counts
            .iter()
            .enumerate()
            .filter(|(_, count)| **count > 0)
            .map(|(index, count)| json!([self.lowest(index), count]))
            .collect();
        json!({
            "significant_digits": self.significant_digits,
            "count": self.count,
            "min": self.min(),
            "max": self.max(),
            "mean": self.mean(),
            "buckets": buckets,
        })
    }
}

#[cfg(test)]
mod tests;
