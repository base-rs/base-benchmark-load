// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

#[test]
fn small_values_are_exact() {
    let mut histogram = Histogram::new(3);
    for value in [0, 1, 2047, 2047] {
        histogram.record(value);
    }
    assert_eq!(histogram.count(), 4);
    assert_eq!(histogram.min(), Some(0));
    assert_eq!(histogram.max(), Some(2047));
    assert_eq!(histogram.value_at_quantile(0.25), Some(0));
    assert_eq!(histogram.value_at_quantile(0.5), Some(1));
    assert_eq!(histogram.value_at_quantile(0.75), Some(2047));
}

#[test]
fn buckets_follow_hdr_bucketing() {
    let histogram = Histogram::new(3);
    assert_eq!(histogram.magnitude, 11);
    // The first doubled bucket starts at 2048 and holds two values.
    assert_eq!(histogram.index(2047), 2047);
    assert_eq!(histogram.index(2048), 2048);
    assert_eq!(histogram.index(2049), 2048);
    assert_eq!(histogram.index(2050), 2049);
    assert_eq!(histogram.lowest(2048), 2048);
    assert_eq!(histogram.highest(2048), 2049);
    // Every bucket's bounds map back to it, and buckets are contiguous.
    for index in [2048, 3071, 3072, 5000, 20_000, 40_000] {
        let (lowest, highest) = (histogram.lowest(index), histogram.highest(index));
        assert_eq!(histogram.index(lowest), index);
        assert_eq!(histogram.index(highest), index);
        assert_eq!(histogram.index(highest + 1), index + 1);
        // Never wider than one part in a thousand.
        assert!(
            (highest - lowest + 1) * 1000 <= lowest,
            "{lowest}..={highest}"
        );
    }
    // The largest value has a bucket, without overflow.
    let last = histogram.index(u64::MAX);
    assert_eq!(histogram.highest(last), u64::MAX);
    assert_eq!(histogram.index(histogram.lowest(last)), last);
}

#[test]
fn precision_sets_the_magnitude() {
    assert_eq!(Histogram::new(1).magnitude, 5);
    assert_eq!(Histogram::new(2).magnitude, 8);
    assert_eq!(Histogram::new(5).magnitude, 18);
    assert_eq!(Histogram::new(2).significant_digits(), 2);
}

#[test]
#[should_panic(expected = "from 1 to 5 significant digits")]
fn rejects_unsupported_precision() {
    Histogram::new(0);
}

#[test]
fn quantiles_report_the_highest_equivalent_value_within_the_range() {
    let mut histogram = Histogram::new(3);
    // 1,000,000 is in a bucket of width 512 starting at 999,936.
    histogram.record(1_000_000);
    histogram.record(1_000_100);
    histogram.record(5_000_000);
    assert_eq!(histogram.value_at_quantile(0.0), Some(1_000_000));
    assert_eq!(histogram.value_at_quantile(0.5), Some(1_000_447));
    assert_eq!(histogram.value_at_quantile(0.99), Some(5_000_000));
    assert_eq!(histogram.value_at_quantile(1.0), Some(5_000_000));
    assert_eq!(histogram.value_at_quantile(2.0), Some(5_000_000));
    assert_eq!(histogram.mean(), Some(7_000_100.0 / 3.0));
}

#[test]
fn a_single_value_is_reported_exactly_at_every_quantile() {
    let mut histogram = Histogram::new(3);
    histogram.record(123_456_789);
    for quantile in [0.0, 0.5, 0.999, 1.0] {
        assert_eq!(histogram.value_at_quantile(quantile), Some(123_456_789));
    }
}

#[test]
fn empty_histograms_have_no_statistics() {
    let histogram = Histogram::new(3);
    assert_eq!(histogram.count(), 0);
    assert_eq!(histogram.min(), None);
    assert_eq!(histogram.max(), None);
    assert_eq!(histogram.mean(), None);
    assert_eq!(histogram.value_at_quantile(0.5), None);
    assert_eq!(
        histogram.to_json(),
        json!({"significant_digits": 3, "count": 0, "min": null, "max": null, "mean": null, "buckets": []})
    );
}

#[test]
fn merging_is_exact() {
    let values: Vec<u64> = (0..10_000u64).map(|i| i * i * 37 % 50_000_000).collect();
    let mut whole = Histogram::new(3);
    let mut parts = [Histogram::new(3), Histogram::new(3), Histogram::new(3)];
    for (i, value) in values.iter().enumerate() {
        whole.record(*value);
        parts[i % 3].record(*value);
    }
    // Merge in an order where a shorter histogram receives a longer one, and the reverse.
    let mut merged = Histogram::new(3);
    merged.merge(&parts[2]);
    merged.merge(&parts[0]);
    merged.merge(&parts[1]);
    assert_eq!(merged, whole);
    for quantile in [0.0, 0.5, 0.9, 0.99, 0.999, 1.0] {
        assert_eq!(
            merged.value_at_quantile(quantile),
            whole.value_at_quantile(quantile)
        );
    }
    let mut short = Histogram::new(3);
    short.record(1);
    let mut long = Histogram::new(3);
    long.record(1 << 40);
    long.merge(&short);
    assert_eq!(long.count(), 2);
    assert_eq!(long.min(), Some(1));
    assert_eq!(long.max(), Some(1 << 40));
}

#[test]
#[should_panic(expected = "same precision")]
fn merging_rejects_different_precision() {
    Histogram::new(3).merge(&Histogram::new(2));
}

#[test]
fn json_lists_non_empty_buckets() {
    let mut histogram = Histogram::new(2);
    histogram.record(3);
    histogram.record(3);
    histogram.record(1000);
    assert_eq!(
        histogram.to_json(),
        json!({
            "significant_digits": 2,
            "count": 3,
            "min": 3,
            "max": 1000,
            "mean": 1006.0 / 3.0,
            "buckets": [[3, 2], [1000, 1]],
        })
    );
}
