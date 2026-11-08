#!/usr/bin/env python3
"""Evaluate base-benchmark-load against wrk and the hello benchmark's fixed-rate client on hana.

Every sample starts a fresh server on CPUs 2,3, runs one client on CPU 1, and holds the
base-benchmark host lock (flock /tmp/base-benchmark-host.lock) for the sample's duration.
"""
import argparse, fcntl, json, os, random, re, select, subprocess, sys, time
from pathlib import Path

ROOT = Path.home() / '.cache/load-eval-20261109'
SERVER = ROOT / 'http/benchmarks/hello/target/release/base-http-hello-benchmark'
LOAD = ROOT / 'load/target/release/base-benchmark-load'
FIXED = ROOT / 'http/benchmarks/hello/fixed-rate-client/target/release/base-http-fixed-rate-client'
WRK = ROOT / 'bin/wrk'
RESPONSE = b'HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nHello, world!'
LOCK = '/tmp/base-benchmark-host.lock'
SERVER_CPUS = '2,3'
CLIENT_CPU = '1'
OTHER = re.compile(r'base-benchmark|\bwrk\b|fixed-rate-client|hello-benchmark|criterion|cargo-bench|cargo bench|\bstress\b|hyperfine')
TICK = os.sysconf('SC_CLK_TCK')


def others():
    """Benchmark-like processes that are not this driver or its ancestors."""
    mine = set()
    pid = os.getpid()
    while pid > 1:
        mine.add(pid)
        try:
            pid = int(Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[1])
        except Exception:
            break
    found = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit() or int(entry.name) in mine:
            continue
        try:
            command = (entry / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace').strip()
        except Exception:
            continue
        if command and OTHER.search(command):
            found.append(f'{entry.name} {command}')
    return found


class Lock:
    def __enter__(self):
        while True:
            busy = others()
            if not busy:
                break
            print(f'waiting for other benchmark processes: {busy}', file=sys.stderr, flush=True)
            time.sleep(15)
        self.file = open(LOCK, 'r')
        waited = time.monotonic()
        fcntl.flock(self.file, fcntl.LOCK_EX)
        self.waited = time.monotonic() - waited
        return self

    def __exit__(self, *_):
        fcntl.flock(self.file, fcntl.LOCK_UN)
        self.file.close()


def process_cpu(pid):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return {'user': int(fields[11]) / TICK, 'system': int(fields[12]) / TICK, 'threads': int(fields[17])}


def cpu_table():
    table = {}
    for line in Path('/proc/stat').read_text().splitlines():
        if re.match(r'cpu\d+ ', line):
            name, *values = line.split()
            table[name] = [int(v) for v in values[:8]]
    return table


def cpu_delta(before, after):
    names = ['user', 'nice', 'system', 'idle', 'iowait', 'irq', 'softirq', 'steal']
    result = {}
    for cpu in before:
        delta = [b - a for a, b in zip(before[cpu], after[cpu])]
        total = sum(delta) or 1
        result[cpu] = {n: round(d / total, 4) for n, d in zip(names, delta)}
    return result


class Server:
    def __init__(self, backend, stderr):
        self.process = subprocess.Popen(['taskset', '-c', SERVER_CPUS, str(SERVER), backend],
                                        stdout=subprocess.PIPE, stderr=stderr, text=True)
        assert select.select([self.process.stdout], [], [], 10)[0], 'server readiness timeout'
        self.address = self.process.stdout.readline().strip()
        assert self.address.startswith('127.0.0.1:'), self.address

    def cpu(self):
        values = process_cpu(self.process.pid)
        for line in Path(f'/proc/{self.process.pid}/io').read_text().splitlines():
            name, value = line.split(':')
            values['io_' + name] = int(value)
        return values

    def stop(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.process.stdout.close()


def run(command, timeout):
    started = time.monotonic()
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    return result, time.monotonic() - started


def wrk_parse(text):
    def ms(value):
        m = re.fullmatch(r'([\d.]+)(us|ms|s)', value)
        return float(m[1]) * {'us': 1e-3, 'ms': 1, 's': 1e3}[m[2]]
    out = {
        'requests': int(re.search(r'(\d+) requests in', text)[1]),
        'requests_per_second': float(re.search(r'Requests/sec:\s+([\d.]+)', text)[1]),
        'socket_errors': re.search(r'Socket errors: (.*)', text)[1] if 'Socket errors' in text else None,
        'non_2xx': 'Non-2xx' in text,
    }
    for q in ('50', '75', '90', '99'):
        out[f'latency_p{q}_ms'] = ms(re.search(rf'^\s*{q}%\s+(\S+)', text, re.M)[1])
    out['latency_mean_ms'] = ms(re.search(r'Latency:?\s+([\d.]+(?:us|ms|s))', text)[1])
    return out


def metric(record, name):
    for m in record['samples'][0]['metrics']:
        if m['name'] == name:
            return m['measurement'].get('value')
    return None


def load_summary(record):
    ext = record['extensions']['base-benchmark-load']
    periods = ext['periods']
    total_outcomes = sum(p['valid'] + sum(p['invalid'].values()) + sum(p['errors'].values()) for p in periods.values())
    names = [m['name'] for m in record['samples'][0]['metrics']]
    out = {
        'operations': record['samples'][0]['operations'],
        'measured': periods['measured'],
        'server_requests': total_outcomes + ext['preflight_requests'],
        'wall_time_ns': metric(record, 'wall_time'),
        'client_saturated': metric(record, 'client_saturated'),
        'worker_busy': [w['busy_fraction'] for w in ext['workers']],
        'worker_cpu_ns': [w['cpu_ns'] for w in ext['workers']],
        'worker_iterations': [w['iterations'] for w in ext['workers']],
        'saturation_reasons': ext['saturation']['reasons'],
        'completed_in_window': metric(record, 'completed_in_window'),
        'sent_in_window': metric(record, 'sent_in_window'),
        'drain_time_ns': metric(record, 'drain_time'),
        'requests_planned': metric(record, 'requests_planned'),
        'errors': metric(record, 'errors'),
        'responses_invalid': metric(record, 'responses_invalid'),
        'connect_errors': metric(record, 'connect_errors'),
    }
    for dist in ('latency', 'round_trip', 'send_delay', 'dispatch_lag', 'service_time'):
        if f'{dist}_p50' in names:
            out[dist] = {q: metric(record, f'{dist}_{q}') for q in ('p50', 'p90', 'p99', 'p999', 'max', 'mean')}
    return out


def sample(a, out, stem, backend, client, connections, rate=None, warmup=2, duration=8):
    row = {'stem': stem, 'backend': backend, 'client': client, 'connections': connections, 'rate': rate,
           'warmup_s': warmup, 'duration_s': duration, 'utc': time.time()}
    with Lock() as lock, (out / f'{stem}.server.stderr').open('w') as server_errors:
        row['lock_wait_s'] = round(lock.waited, 3)
        row['loadavg_before'] = Path('/proc/loadavg').read_text().strip()
        server = Server(backend, server_errors)
        row['address'] = server.address
        try:
            if client == 'wrk':
                url = f'http://{server.address}/'
                base = ['taskset', '-c', CLIENT_CPU, str(WRK), '-t1', f'-c{connections}', '--latency', '--timeout', '2s']
                warm, _ = run([*base, f'-d{warmup}s', url], warmup + 20)
                (out / f'{stem}.warmup.txt').write_text(warm.stdout + warm.stderr)
                assert warm.returncode == 0, warm.stderr
                cpu_before, server_before = cpu_table(), server.cpu()
                result, elapsed = run([*base, f'-d{duration}s', url], duration + 20)
                server_after, cpu_after = server.cpu(), cpu_table()
                (out / f'{stem}.wrk.txt').write_text(result.stdout + result.stderr)
                assert result.returncode == 0, result.stderr
                parsed = wrk_parse(result.stdout)
                row['wrk'] = parsed
                row['throughput'] = parsed['requests_per_second']
                row['server_requests'] = parsed['requests']
            else:
                if client == 'load':
                    command = ['taskset', '-c', CLIENT_CPU, str(LOAD), a.mode, '--address', server.address,
                               '--connections', str(connections), '--cpus', CLIENT_CPU,
                               '--warmup', f'{warmup}s', '--duration', f'{duration}s', *a.load_args]
                    if rate:
                        command += ['--rate', str(rate)]
                else:
                    command = ['taskset', '-c', CLIENT_CPU, str(FIXED), server.address, str(connections), str(rate),
                               str(warmup), str(duration), CLIENT_CPU]
                cpu_before, server_before = cpu_table(), server.cpu()
                result, elapsed = run(command, warmup + duration + 40)
                server_after, cpu_after = server.cpu(), cpu_table()
                (out / f'{stem}.json').write_text(result.stdout)
                (out / f'{stem}.stderr').write_text(result.stderr)
                row['exit'] = result.returncode
                if result.returncode != 0:
                    print(f'{stem}: client exited {result.returncode}: {result.stderr[-2000:]}', flush=True)
                    row['failed'] = True
                else:
                    record = json.loads(result.stdout)
                    if client == 'load':
                        summary = load_summary(record)
                        row['load'] = summary
                        row['throughput'] = summary['completed_in_window'] / duration
                        row['throughput_from_wall_time'] = summary['operations'] / (summary['wall_time_ns'] / 1e9)
                        row['server_requests'] = summary['server_requests']
                    else:
                        keep = {k: v for k, v in record.items() if k not in ('per_second_scheduled_latency',)}
                        row['fixed'] = keep
                        row['throughput'] = record['achieved_rate']
                        row['server_requests'] = record['total_responses_validated']
            row['client_elapsed_s'] = elapsed
            row['server_cpu_s'] = {k: server_after[k] - server_before[k] for k in ('user', 'system')}
            row['server_io'] = {k: server_after[k] - server_before[k] for k in server_after if k.startswith('io_')}
            row['server_threads'] = server_after['threads']
            row['cpu'] = cpu_delta(cpu_before, cpu_after)
            if row.get('server_requests'):
                total = row['server_cpu_s']['user'] + row['server_cpu_s']['system']
                row['server_cpu_us_per_request'] = total * 1e6 / row['server_requests']
                row['server_cpu_fraction'] = total / elapsed
        finally:
            server.stop()
            row['loadavg_after'] = Path('/proc/loadavg').read_text().strip()
    return row


def describe(row):
    parts = [f"{row['stem']:40}", f"tput {row.get('throughput', 0):10.0f}/s"]
    if 'server_cpu_us_per_request' in row:
        parts.append(f"srv {row['server_cpu_us_per_request']:.3f} us/req ({row['server_cpu_fraction']*100:.0f}%)")
    if 'load' in row:
        l = row['load']
        d = l.get('latency') or l.get('service_time')
        parts.append(f"p50 {d['p50']/1e3:.1f} p99 {d['p99']/1e3:.1f} p999 {d['p999']/1e3:.1f} us")
        parts.append(f"lag99 {l['dispatch_lag']['p99']/1e3:.2f} us busy {l['worker_busy'][0]:.3f} sat {l['client_saturated']}")
    if 'fixed' in row:
        f = row['fixed']
        parts.append(f"p50 {f['scheduled_latency']['p50_us']:.1f} p99 {f['scheduled_latency']['p99_us']:.1f} p999 {f['scheduled_latency']['p999_us']:.1f} us")
        parts.append(f"lag99 {f['dispatch_lag']['p99_us']:.2f} us")
    if 'wrk' in row:
        w = row['wrk']
        parts.append(f"p50 {w['latency_p50_ms']*1e3:.1f} p99 {w['latency_p99_ms']*1e3:.1f} us")
    parts.append(f"cpu1 {row['cpu']['cpu1']['softirq']:.2f}si")
    return ' '.join(parts)


def main():
    p = argparse.ArgumentParser()
    p.add_argument('experiment', choices=['closed', 'rate'])
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--backends', nargs='+', required=True)
    p.add_argument('--clients', nargs='+', required=True)
    p.add_argument('--connections', nargs='+', type=int, default=[64])
    p.add_argument('--rates', nargs='+', type=int, default=[None])
    p.add_argument('--repetitions', type=int, default=5)
    p.add_argument('--warmup', type=int, default=2)
    p.add_argument('--duration', type=int, default=8)
    p.add_argument('--seed', type=int, default=20261109)
    p.add_argument('--load-arg', dest='load_args', action='append', default=[])
    a = p.parse_args()
    a.mode = a.experiment
    a.output.mkdir(parents=True, exist_ok=False)
    (a.output / 'response.bin').write_bytes(RESPONSE)
    if a.experiment == 'rate' and 'load' in a.clients and not any(x.startswith('--response-file') for x in a.load_args):
        a.load_args += ['--response-file', str(a.output / 'response.bin')]
    environment = {'argv': sys.argv, 'utc': time.time(), 'uname': list(os.uname()),
                   'loadavg': Path('/proc/loadavg').read_text(),
                   'governors': sorted(set(Path(f'/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_governor').read_text().strip() for i in range(8))),
                   'top': subprocess.run(['top', '-bn1', '-o', '%CPU', '-w', '200'], capture_output=True, text=True).stdout.splitlines()[:25]}
    (a.output / 'environment.json').write_text(json.dumps(environment, indent=2))
    rng = random.Random(a.seed)
    rows = []
    with (a.output / 'samples.jsonl').open('w') as journal:
        for rep in range(1, a.repetitions + 1):
            groups = [(b, c, r) for b in a.backends for c in a.connections for r in a.rates]
            rng.shuffle(groups)
            for index, (backend, connections, rate) in enumerate(groups):
                clients = list(a.clients)
                if (rep + index) % 2:
                    clients.reverse()
                for client in clients:
                    stem = f"{backend}-{client}-c{connections}" + (f"-r{rate}" if rate else '') + f"-n{rep}"
                    row = sample(a, a.output, stem, backend, client, connections, rate, a.warmup, a.duration)
                    row['repetition'] = rep
                    rows.append(row)
                    journal.write(json.dumps(row) + '\n')
                    journal.flush()
                    print(describe(row) if not row.get('failed') else f"{stem} FAILED", flush=True)


if __name__ == '__main__':
    main()
