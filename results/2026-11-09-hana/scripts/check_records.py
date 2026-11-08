#!/usr/bin/env python3
"""Semantic checks of base-benchmark-load records against base-benchmark's expectations."""
import collections, json, statistics, sys
from pathlib import Path

RAW = Path(sys.argv[1])
problems = []
notes = collections.Counter()


def metrics(record):
    return {m['name']: m for m in record['samples'][0]['metrics']}


def value(record, name):
    return metrics(record)[name]['measurement'].get('value')


rows = {}
for experiment in ('closed', 'rate', 'sweep', 'saturation'):
    for line in (RAW / experiment / 'samples.jsonl').read_text().splitlines():
        row = json.loads(line)
        rows[(experiment, row['stem'])] = row

records = []
for path in sorted(RAW.glob('*/*-load-*.json')) + sorted(RAW.glob('stall/*load.stdout')) + [RAW / 'corrupt/corrupt-one-load.stdout']:
    if path.stat().st_size == 0:
        continue
    record = json.loads(path.read_text())
    records.append((path, record))
    ext = record['extensions']['base-benchmark-load']
    sample = record['samples'][0]
    ops = sample['operations']
    measured = ext['periods']['measured']
    m = metrics(record)
    label = f'{path.parent.name}/{path.name}'
    # 1. operations are the measured period's valid responses, and responses_valid equals them.
    if ops != measured['valid']:
        problems.append(f'{label}: operations {ops} != measured valid {measured["valid"]}')
    if value(record, 'responses_valid') != ops:
        problems.append(f'{label}: responses_valid != operations')
    # 2. counts reconcile.
    invalid = sum(measured['invalid'].values())
    errors = sum(measured['errors'].values())
    if value(record, 'responses_invalid') != invalid or value(record, 'errors') != errors:
        problems.append(f'{label}: invalid/error metrics disagree with periods')
    hist = ext['histograms']
    main = 'latency' if record['parameters']['mode'] == 'rate' else 'service_time'
    if hist[main]['count'] != ops:
        problems.append(f'{label}: {main} histogram count {hist[main]["count"]} != operations {ops}')
    if record['parameters']['mode'] == 'rate':
        p = record['parameters']
        planned = p['rate'] * p['duration_ns'] // 10**9
        if value(record, 'requests_planned') != planned or ops + invalid + errors != planned:
            problems.append(f'{label}: cohort {ops}+{invalid}+{errors} vs planned {value(record, "requests_planned")} (expected {planned})')
        lm, rm, sm = value(record, 'latency_mean'), value(record, 'round_trip_mean'), value(record, 'send_delay_mean')
        if abs(lm - rm - sm) > 0.001 * lm + 5:
            problems.append(f'{label}: latency_mean {lm} != round_trip_mean {rm} + send_delay_mean {sm}')
        notes['rate records with exact cohort and mean decomposition'] += 1
    # 3. quantiles monotone.
    for d in ('latency', 'round_trip', 'send_delay', 'dispatch_lag', 'service_time'):
        if f'{d}_p50' in m:
            q = [value(record, f'{d}_{s}') for s in ('p50', 'p90', 'p99', 'p999', 'max')]
            if q != sorted(q):
                problems.append(f'{label}: {d} quantiles not monotone {q}')
    # 4. wall time against the configured window.
    duration = record['parameters']['duration_ns']
    notes.setdefault('wall', [])
    notes['wall'].append(value(record, 'wall_time') - duration)

# 5. Parameters equal across runs with different ports, signature equal across subjects, as base-benchmark's Expectations check.
groups = collections.defaultdict(list)
for path, record in records:
    if path.parent.name in ('closed', 'rate', 'sweep', 'saturation'):
        p = record['parameters']
        groups[(path.parent.name, p['mode'], p['connections'], p['rate'])].append((path, record))
group_report = []
for key, items in sorted(groups.items(), key=str):
    params = {json.dumps(r['parameters'], sort_keys=True) for _, r in items}
    signatures = {r['signature'] for _, r in items}
    workloads = {r['workload'] for _, r in items}
    addresses = {r['extensions']['base-benchmark-load']['context']['address'] for _, r in items}
    backends = {p.name.split('-load-')[0] for p, _ in items}
    group_report.append({'group': key, 'records': len(items), 'backends': sorted(backends), 'distinct_addresses': len(addresses),
                         'distinct_parameters': len(params), 'distinct_signatures': len(signatures), 'workloads': sorted(workloads)})
    if len(params) != 1 or len(signatures) != 1 or len(workloads) != 1:
        problems.append(f'{key}: {len(params)} parameter sets, {len(signatures)} signatures across {len(addresses)} addresses')

# 6. Independent server count: hyper-tokio writes each response with one writev, counted in /proc/PID/io syscw.
server_counts = []
for (experiment, stem), row in rows.items():
    if row['backend'] == 'hyper-tokio' and 'server_io' in row and not row.get('failed'):
        server_counts.append({'stem': stem, 'client': row['client'], 'client_count': row['server_requests'],
                              'server_writes': row['server_io']['io_syscw'],
                              'difference': row['server_io']['io_syscw'] - row['server_requests']})

# 7. Throughput from wall_time (as base-benchmark computes it) against the exact window count.
throughput = []
for (experiment, stem), row in rows.items():
    if row['client'] == 'load' and not row.get('failed'):
        throughput.append(row['throughput_from_wall_time'] / (row['load']['operations'] / row['duration_s']) - 1)

walls = notes.pop('wall')
print(json.dumps({
    'records_checked': len(records),
    'problems': problems,
    'notes': dict(notes),
    'wall_time_minus_duration_ns': {'min': min(walls), 'median': statistics.median(walls), 'max': max(walls)},
    'throughput_from_wall_time_relative_error': {'min': min(throughput), 'median': statistics.median(throughput), 'max': max(throughput)} if throughput else None,
    'groups': group_report,
    'hyper_tokio_server_writes_vs_client_counts': server_counts,
}, indent=1))
