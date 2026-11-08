#!/usr/bin/env python3
"""Tabulate measure.py sweep samples: medians with min–max, per client and rate."""
import json, statistics, sys
from collections import defaultdict

rows = [json.loads(line) for line in open(sys.argv[1])]
groups = defaultdict(list)
for row in rows:
    groups[(row['rate'], row['client'])].append(row)

KEYS = [('achieved', 'Achieved req/s', 0), ('shortfall', 'Planned − sent in window', 0),
        ('latency_p50_us', 'p50 µs', 1), ('latency_p99_us', 'p99 µs', 1), ('latency_p999_us', 'p99.9 µs', 1),
        ('dispatch_lag_p99_us', 'Dispatch lag p99 µs', 1), ('schedule_lag_p99_us', 'Schedule lag p99 µs', 1),
        ('busy', 'Busy', 3), ('softirq_share', 'Softirq share', 3)]


def cell(values, digits):
    values = [value for value in values if value is not None]
    if not values:
        return '–'
    def fmt(value):
        return f'{value:,.{digits}f}'
    if len(values) == 1:
        return fmt(values[0])
    return f'{fmt(statistics.median(values))} ({fmt(min(values))}–{fmt(max(values))})'


print('| Offered | Client | ' + ' | '.join(title for _, title, _ in KEYS) + ' | Flagged | Load average |')
print('|---:|---|' + '---:|' * len(KEYS) + '---:|---|')
for (rate, client), samples in sorted(groups.items()):
    summaries = []
    for row in samples:
        summary = dict(row.get('summary') or {})
        if summary:
            summary['shortfall'] = summary['planned'] - summary['sent_in_window']
            if summary.get('softirq_ns') is not None:
                summary['softirq_share'] = summary['softirq_ns'] / 4e9
        summaries.append(summary)
    cells = [cell([summary.get(key) for summary in summaries], digits) for key, _, digits in KEYS]
    flagged = '–' if client == 'fixed' else f"{sum(1 for summary in summaries if summary.get('saturated'))}/{len(summaries)}"
    loads = [float(row['loadavg'][0]) for row in samples]
    print(f'| {rate // 1000}k | {client} | ' + ' | '.join(cells) + f' | {flagged} | {min(loads):.1f}–{max(loads):.1f} |')

print()
print('Reasons (after):')
for (rate, client), samples in sorted(groups.items()):
    if client != 'after':
        continue
    for row in samples:
        print(f"- {rate // 1000}k round {row['round']}: {'; '.join(row['summary']['reasons']) or 'not saturated'}")
