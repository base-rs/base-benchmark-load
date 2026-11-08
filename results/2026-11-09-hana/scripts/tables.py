#!/usr/bin/env python3
"""Markdown tables from the samples.jsonl files: medians over repetitions, with min–max ranges."""
import collections, json, statistics, sys
from pathlib import Path

RAW = Path(sys.argv[1])


def load(experiment):
    return [json.loads(l) for l in (RAW / experiment / 'samples.jsonl').read_text().splitlines()]


def med(values):
    return statistics.median(values)


def rng(values, fmt):
    return f"{fmt(med(values))} ({fmt(min(values))}–{fmt(max(values))})"


def us(ns):
    return ns / 1e3


def server_cpu_with_softirq(row):
    """Server process CPU plus softirq on its CPUs 2,3, per request. /proc/PID/stat excludes softirq time."""
    elapsed = row['client_elapsed_s']
    softirq = (row['cpu']['cpu2']['softirq'] + row['cpu']['cpu3']['softirq']) * elapsed
    total = row['server_cpu_s']['user'] + row['server_cpu_s']['system'] + softirq
    return total * 1e6 / row['server_requests']


def closed():
    rows = load('closed')
    out = ['| Backend | Conns | Client | Throughput req/s | Server CPU µs/req (process) | + softirq on CPUs 2,3 | p50 µs | p99 µs | Load/wrk throughput, paired |', '|---|---:|---|---:|---:|---:|---:|---:|---:|']
    cells = collections.defaultdict(list)
    for r in rows:
        cells[(r['backend'], r['connections'], r['client'])].append(r)
    for backend in ('base-native', 'hyper-tokio'):
        for c in (1, 16, 64):
            pairs = {}
            for client in ('load', 'wrk'):
                g = sorted(cells[(backend, c, client)], key=lambda r: r['repetition'])
                pairs[client] = g
                if client == 'load':
                    p50 = [us(r['load']['service_time']['p50']) for r in g]
                    p99 = [us(r['load']['service_time']['p99']) for r in g]
                else:
                    p50 = [r['wrk']['latency_p50_ms'] * 1e3 for r in g]
                    p99 = [r['wrk']['latency_p99_ms'] * 1e3 for r in g]
                ratio = ''
                if client == 'wrk':
                    values = [a['throughput'] / b['throughput'] - 1 for a, b in zip(pairs['load'], g)]
                    ratio = rng([v * 100 for v in values], lambda v: f'{v:+.1f}%')
                out.append(f"| {backend} | {c} | {'base-benchmark-load' if client == 'load' else 'wrk'} | "
                           f"{rng([r['throughput'] for r in g], lambda v: f'{v:,.0f}')} | "
                           f"{rng([r['server_cpu_us_per_request'] for r in g], lambda v: f'{v:.2f}')} | "
                           f"{med([server_cpu_with_softirq(r) for r in g]):.2f} | "
                           f"{med(p50):.1f} | {med(p99):.1f} | {ratio} |")
    return '\n'.join(out)


def rate(experiment, backends, rates, reps_note):
    rows = load(experiment)
    cells = collections.defaultdict(list)
    for r in rows:
        cells[(r['backend'], r['rate'], r['client'])].append(r)
    out = [f'{reps_note}', '', '| Backend | Offered | Client | Achieved req/s | Latency p50 µs | p99 µs | p99.9 µs | Send delay p99 µs | Dispatch lag p99 µs | Busy | Saturated |',
           '|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|']
    for backend in backends:
        for rate_ in rates:
            for client in ('load', 'fixed'):
                g = cells[(backend, rate_, client)]
                if not g:
                    continue
                if client == 'load':
                    lat = lambda r, q: us(r['load']['latency'][q])
                    sd = [us(r['load']['send_delay']['p99']) for r in g]
                    lag = [us(r['load']['dispatch_lag']['p99']) for r in g]
                    busy = f"{med([r['load']['worker_busy'][0] for r in g]):.3f}"
                    sat = f"{sum(int(r['load']['client_saturated']) for r in g)}/{len(g)}"
                else:
                    keys = {'p50': 'p50_us', 'p99': 'p99_us', 'p999': 'p999_us'}
                    lat = lambda r, q: r['fixed']['scheduled_latency'][keys[q]]
                    sd = [r['fixed']['send_delay']['p99_us'] for r in g]
                    lag = [r['fixed']['dispatch_lag']['p99_us'] for r in g]
                    busy, sat = '–', '–'
                f1 = lambda v: f'{v:,.1f}'
                out.append(f"| {backend} | {rate_ // 1000}k | {'base-benchmark-load' if client == 'load' else 'fixed-rate-client'} | "
                           f"{rng([r['throughput'] for r in g], lambda v: f'{v:,.0f}')} | "
                           f"{rng([lat(r, 'p50') for r in g], f1)} | {rng([lat(r, 'p99') for r in g], f1)} | {rng([lat(r, 'p999') for r in g], f1)} | "
                           f"{med(sd):,.1f} | {med(lag):,.1f} | {busy} | {sat} |")
    return '\n'.join(out)


print('## Closed loop (5 rounds, 2 s warmup, 8 s window; medians, range in parentheses)\n')
print(closed())
print('\n## Open loop\n')
print(rate('rate', ['base-iouring-raw', 'tokio-task-raw'], [100000, 140000, 160000, 200000], '5 rounds, 64 connections, depth 1, 2 s warmup, 8 s window.'))
print('\n## Client saturation sweep\n')
print(rate('saturation', ['base-iouring-raw'], [120000, 150000, 170000, 190000, 205000, 215000, 230000], '3 rounds, base-iouring-raw, 1 s warmup, 4 s window.'))
