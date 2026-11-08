#!/usr/bin/env python3
"""Which runs each build flagged as saturated: flags.py SAMPLES.jsonl...

A run whose 99th percentile latency exceeds 1 ms here was disturbed, by the client's limits or
by another process on its CPU; one whose 99th percentile is at most 100 µs was not.
"""
import json, sys

rows = [json.loads(line) for path in sys.argv[1:] for line in open(path)]
for client in ('before', 'after'):
    runs = [row for row in rows if row['client'] == client and row['exit'] == 0]
    disturbed = [row for row in runs if row['summary']['latency_p99_us'] > 1000]
    calm = [row for row in runs if row['summary']['latency_p99_us'] <= 100]
    missed = [row for row in disturbed if not row['summary']['saturated']]
    flagged = [row for row in calm if row['summary']['saturated']]
    print(f'{client}: {len(runs)} runs; {len(disturbed)} with p99 over 1 ms, {len(missed)} of them not flagged; '
          f'{len(calm)} with p99 at most 100 µs, {len(flagged)} of them flagged')
    for row in missed:
        summary = row['summary']
        print(f"  missed: {row['rate'] // 1000}k p99 {summary['latency_p99_us']:,.0f} µs, busy {summary['busy']:.3f}, "
              f"{summary['planned'] - summary['sent_in_window']:,} planned in the window not sent in it")
    for row in flagged:
        summary = row['summary']
        print(f"  flagged: {row['rate'] // 1000}k p99 {summary['latency_p99_us']:.1f} µs: {'; '.join(summary['reasons'])}")
