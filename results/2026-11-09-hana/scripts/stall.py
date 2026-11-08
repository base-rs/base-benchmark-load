#!/usr/bin/env python3
"""validate.py's stall, healthy and corrupt cases, for both the fixed-rate client and base-benchmark-load.

Differences from benchmarks/hello/fixed-rate-client/validate.py: the server accepts connections until
it is closed (base-benchmark-load opens one more connection for its preflight request), and a stall can
also be placed on one global request number, to put it inside a measured window after a warmup.
"""
import fcntl, json, socket, subprocess, sys, threading, time
from pathlib import Path

ROOT = Path.home() / '.cache/load-eval-20261109'
LOAD = ROOT / 'load/target/release/base-benchmark-load'
FIXED = ROOT / 'http/benchmarks/hello/fixed-rate-client/target/release/base-http-fixed-rate-client'
RESPONSE = b'HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nHello, world!'
OUT = ROOT / ('out/' + (sys.argv[1] if len(sys.argv) > 1 else 'stall'))


class Server:
    def __init__(self, delay=0.0, first_per_connection=False, stall_at=None, corrupt=False, corrupt_at=None):
        self.delay, self.first, self.stall_at, self.corrupt, self.corrupt_at = delay, first_per_connection, stall_at, corrupt, corrupt_at
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(('127.0.0.1', 0))
        self.listener.listen(128)
        self.host, self.port = self.listener.getsockname()
        self.count = 0
        self.lock = threading.Lock()
        self.errors = []
        self.stalled = []
        self.connections = 0
        self.threads = []
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while True:
            try:
                connection, _ = self.listener.accept()
            except OSError:
                return
            self.connections += 1
            t = threading.Thread(target=self.serve, args=(connection,), daemon=True)
            t.start()
            self.threads.append(t)

    def serve(self, connection):
        try:
            with connection:
                connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                pending, first = b'', True
                while True:
                    data = connection.recv(65536)
                    if not data:
                        break
                    pending += data
                    while b'\r\n\r\n' in pending:
                        request, pending = pending.split(b'\r\n\r\n', 1)
                        assert request == f'GET / HTTP/1.1\r\nHost: {self.host}:{self.port}'.encode(), request
                        with self.lock:
                            self.count += 1
                            number = self.count
                        if (self.first and first) or (self.stall_at is not None and number == self.stall_at):
                            self.stalled.append((number, time.monotonic()))
                            time.sleep(self.delay)
                        first = False
                        connection.sendall(RESPONSE[:-1] + b'?' if self.corrupt or number == self.corrupt_at else RESPONSE)
        except Exception as e:
            self.errors.append(repr(e))

    def close(self):
        self.listener.close()
        for t in self.threads:
            t.join(5)


def run_client(kind, server, connections, rate, warmup, duration, cpus, extra=()):
    address = f'{server.host}:{server.port}'
    if kind == 'fixed':
        command = ['taskset', '-c', '1', str(FIXED), address, str(connections), str(rate), str(warmup), str(duration), cpus]
    else:
        command = ['taskset', '-c', '1', str(LOAD), 'rate', '--address', address, '--rate', str(rate),
                   '--connections', str(connections), '--cpus', cpus, '--warmup', f'{warmup}s',
                   '--duration', f'{duration}s', *extra]
    return command, subprocess.run(command, capture_output=True, text=True, timeout=60)


def summarize(kind, stdout):
    r = json.loads(stdout)
    if kind == 'fixed':
        return {'measured_valid': r['measured_responses_validated'], 'total_valid': r['total_responses_validated'],
                'latency_p99_us': r['scheduled_latency']['p99_us'], 'latency_p999_us': r['scheduled_latency']['p999_us'],
                'latency_max_us': r['scheduled_latency']['max_us'], 'send_delay_max_us': r['send_delay']['max_us'],
                'round_trip_max_us': r['wire_rtt']['max_us'], 'dispatch_lag_max_us': r['dispatch_lag']['max_us'],
                'per_second_max_us': [s['max_us'] for s in r['per_second_scheduled_latency']]}
    m = {x['name']: x['measurement'].get('value') for x in r['samples'][0]['metrics']}
    e = r['extensions']['base-benchmark-load']
    return {'operations': r['samples'][0]['operations'], 'planned': e['planned'] if 'planned' in e else None,
            'periods': {k: v['valid'] for k, v in e['periods'].items()},
            'errors': m.get('errors'), 'invalid': m.get('responses_invalid'),
            'latency_p99_us': m['latency_p99'] / 1e3, 'latency_p999_us': m['latency_p999'] / 1e3,
            'latency_max_us': m['latency_max'] / 1e3, 'send_delay_max_us': m['send_delay_max'] / 1e3,
            'round_trip_max_us': m['round_trip_max'] / 1e3, 'dispatch_lag_max_us': m['dispatch_lag_max'] / 1e3,
            'intervals_max_us': [i['latency_max_ns'] / 1e3 for i in e['intervals']],
            'client_saturated': m.get('client_saturated')}


def main():
    OUT.mkdir(parents=True, exist_ok=False)
    (OUT / 'response.bin').write_bytes(RESPONSE)
    cases = [
        # validate.py's three cases: 1000/s, no warmup, 1 s.
        ('healthy', dict(), 4, 1000, 0, 1, '1', ()),
        ('stall-first', dict(delay=0.04, first_per_connection=True), 1, 1000, 0, 1, '1', ()),
        ('stall-first-depth2', dict(delay=0.04, first_per_connection=True), 1, 1000, 0, 1, '1', ('--depth', '2')),
        ('corrupt', dict(corrupt=True), 1, 1000, 0, 1, '1', ()),
        ('corrupt-fail-on', dict(corrupt=True), 1, 1000, 0, 1, '1', ('--fail-on', 'invalid')),
        # A 40 ms stall in the middle of the measured window: 4 connections at 4000/s, 1 s warmup, 2 s window.
        # Request 6000 is planned at 1.5 s (preflight is request 1 for base-benchmark-load, so it is ordinal 5998 or 5999).
        ('stall-mid', dict(delay=0.04, stall_at=6000), 4, 4000, 1, 2, '1', ()),
        ('stall-mid-depth4', dict(delay=0.04, stall_at=6000), 4, 4000, 1, 2, '1', ('--depth', '4')),
    ]
    if OUT.name == 'corrupt':
        response = OUT / 'response.bin'
        cases = [
            ('corrupt-response-file', dict(corrupt=True), 1, 1000, 0, 1, '1', ('--response-file', str(response))),
            ('corrupt-body', dict(corrupt=True), 1, 1000, 0, 1, '1', ('--body', 'Hello, world!')),
            ('corrupt-one', dict(corrupt_at=500), 1, 1000, 0, 1, '1', ()),
            ('corrupt-one-fail-on', dict(corrupt_at=500), 1, 1000, 0, 1, '1', ('--fail-on', 'invalid')),
        ]
    results = []
    for name, server_options, connections, rate, warmup, duration, cpus, extra in cases:
        for kind in ('fixed', 'load'):
            if kind == 'fixed' and (extra or name.endswith('fail-on')):
                continue
            with open('/tmp/base-benchmark-host.lock') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                server = Server(**server_options)
                command, result = run_client(kind, server, connections, rate, warmup, duration, cpus, extra)
                server.close()
            stem = f'{name}-{kind}'
            (OUT / f'{stem}.stdout').write_text(result.stdout)
            (OUT / f'{stem}.stderr').write_text(result.stderr)
            row = {'case': name, 'client': kind, 'command': command, 'exit': result.returncode,
                   'server_requests': server.count, 'server_connections': server.connections,
                   'server_errors': server.errors, 'stalled_request_numbers': [n for n, _ in server.stalled]}
            if result.returncode == 0 and result.stdout.strip():
                row.update(summarize(kind, result.stdout))
            else:
                row['stderr_tail'] = result.stderr[-600:]
            results.append(row)
            print(json.dumps({k: v for k, v in row.items() if k != 'command'}), flush=True)
    (OUT / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')


if __name__ == '__main__':
    main()
