#!/usr/bin/env python3
"""Screen base-benchmark-load before and after the saturation changes on this host.

Every sample starts a fresh hello-benchmark server on SERVER_CPUS, runs one client on
CLIENT_CPU (controller on CONTROLLER_CPU), and holds the base-benchmark host lock
(flock /tmp/base-benchmark-host.lock) while it runs.

  measure.py strace OUTPUT            strace -f -c of each client at 20k/s, 64 connections
  measure.py sweep OUTPUT ROUNDS RATE...  open-loop sweep, rounds interleaved

Binaries are taken from the directory given by $MEASURE_BIN: server, load-before,
load-after, fixed.
"""
import fcntl, json, os, random, select, subprocess, sys, time
from pathlib import Path

BIN = Path(os.environ['MEASURE_BIN'])
SERVER_CPUS = os.environ.get('SERVER_CPUS', '2,3')
CLIENT_CPU = os.environ.get('CLIENT_CPU', '4')
CONTROLLER_CPU = os.environ.get('CONTROLLER_CPU', '6')
BACKEND = os.environ.get('BACKEND', 'base-iouring-raw')
LOCK = '/tmp/base-benchmark-host.lock'


class Server:
    def __enter__(self):
        self.process = subprocess.Popen(['taskset', '-c', SERVER_CPUS, str(BIN / 'server'), BACKEND],
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        assert select.select([self.process.stdout], [], [], 10)[0], 'server readiness timeout'
        self.address = self.process.stdout.readline().strip()
        assert self.address.startswith('127.0.0.1:'), self.address
        return self

    def __exit__(self, *_):
        self.process.terminate()
        self.process.wait(timeout=10)
        self.process.stdout.close()


def locked(function):
    with open(LOCK, 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            return function()
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


def command(client, address, rate, warmup, duration):
    if client == 'fixed':
        return ['taskset', '-c', CONTROLLER_CPU, str(BIN / 'fixed'), address, '64', str(rate),
                str(warmup), str(duration), CLIENT_CPU]
    return [str(BIN / f'load-{client}'), 'rate', '--address', address, '--rate', str(rate),
            '--connections', '64', '--cpus', CLIENT_CPU, '--controller-cpu', CONTROLLER_CPU,
            '--warmup', f'{warmup}s', '--duration', f'{duration}s']


def metric(record, name):
    for entry in record['samples'][0]['metrics']:
        if entry['name'] == name:
            return entry['measurement'].get('value')
    return None


def summarize(client, stdout):
    if client == 'fixed':
        result = json.loads(stdout)
        return {
            'achieved': result['achieved_rate'],
            'planned': result['planned_measured_requests'],
            'sent_in_window': result['sent_in_window'],
            'latency_p50_us': result['scheduled_latency']['p50_us'],
            'latency_p99_us': result['scheduled_latency']['p99_us'],
            'latency_p999_us': result['scheduled_latency']['p999_us'],
            'dispatch_lag_p99_us': result['dispatch_lag']['p99_us'],
        }
    record = json.loads(stdout)
    extension = record['extensions']['base-benchmark-load']
    worker = extension['workers'][0]
    seconds = metric(record, 'wall_time') / 1e9
    summary = {
        'achieved': record['samples'][0]['operations'] / seconds,
        'planned': metric(record, 'requests_planned'),
        'sent_in_window': metric(record, 'sent_in_window'),
        'latency_p50_us': metric(record, 'latency_p50') / 1e3,
        'latency_p99_us': metric(record, 'latency_p99') / 1e3,
        'latency_p999_us': metric(record, 'latency_p999') / 1e3,
        'dispatch_lag_p99_us': metric(record, 'dispatch_lag_p99') / 1e3,
        'busy': worker['busy_fraction'],
        'cpu_ns': worker['cpu_ns'],
        'saturated': extension['saturation']['saturated'],
        'reasons': extension['saturation']['reasons'],
    }
    if metric(record, 'schedule_lag_p99') is not None:
        summary['schedule_lag_p99_us'] = metric(record, 'schedule_lag_p99') / 1e3
        summary['schedule_lag_p999_us'] = metric(record, 'schedule_lag_p999') / 1e3
        summary['softirq_ns'] = worker['softirq_ns']
    return summary


def sample(client, rate, warmup, duration, wrap=()):
    def body():
        with Server() as server:
            loadavg = Path('/proc/loadavg').read_text().split()[:3]
            result = subprocess.run([*wrap, *command(client, server.address, rate, warmup, duration)],
                                    capture_output=True, text=True, timeout=120)
            return loadavg, result
    loadavg, result = locked(body)
    return {'client': client, 'rate': rate, 'exit': result.returncode, 'loadavg': loadavg,
            'stdout': result.stdout, 'stderr': result.stderr}


def strace(output):
    output.mkdir(parents=True)
    for client in ['before', 'after', 'fixed']:
        counts = output / f'{client}.txt'
        row = sample(client, 20000, 1, 2, wrap=['strace', '-f', '-c', '-o', str(counts)])
        assert row['exit'] == 0, row['stderr']
        (output / f'{client}.stdout').write_text(row['stdout'])
        print(client, 'done', file=sys.stderr)


def sweep(output, rounds, rates):
    output.mkdir(parents=True)
    plan = [(round_, client, rate) for round_ in range(1, rounds + 1)
            for client in ['before', 'after', 'fixed'] for rate in rates]
    random.Random(20261109).shuffle(plan)
    plan.sort(key=lambda entry: entry[0])
    with open(output / 'samples.jsonl', 'w') as samples:
        for round_, client, rate in plan:
            row = sample(client, rate, 1, 4)
            row['round'] = round_
            if row['exit'] == 0:
                row['summary'] = summarize(client, row['stdout'])
            (output / f'{client}-{rate}-{round_}.stdout').write_text(row.pop('stdout'))
            (output / f'{client}-{rate}-{round_}.stderr').write_text(row.pop('stderr'))
            samples.write(json.dumps(row) + '\n')
            samples.flush()
            print(round_, client, rate, row['exit'], row.get('summary', {}).get('latency_p99_us'), file=sys.stderr)


if __name__ == '__main__':
    mode, output = sys.argv[1], Path(sys.argv[2])
    if mode == 'strace':
        strace(output)
    else:
        sweep(output, int(sys.argv[3]), [int(rate) for rate in sys.argv[4:]])
