#!/usr/bin/env python3
"""Reduce the evaluation's raw output to what is committed: trim.py RAW OUTPUT.

Keeps every sample row of the summarized experiments, without the per-CPU times of CPUs other
than the client's (1) and the server's (2, 3), without the server's /proc/PID/io counters, and
without home directories in commands; the stall and corruption summaries; the strace counts; and
each experiment's environment without its process list. The per-sample client outputs, about
30 MB, are left out; `tables.py OUTPUT` regenerates `tables.md` from what is kept.
"""
import json, sys
from pathlib import Path

RAW, OUTPUT = Path(sys.argv[1]), Path(sys.argv[2])
EXPERIMENTS = ['closed', 'closed-servercount', 'rate', 'saturation', 'sweep']
HOME = str(Path.home())


def scrub(value):
    if isinstance(value, str):
        return value.replace(HOME, '~')
    if isinstance(value, list):
        return [scrub(item) for item in value]
    if isinstance(value, dict):
        return {key: scrub(item) for key, item in value.items()}
    return value


for experiment in EXPERIMENTS:
    (OUTPUT / experiment).mkdir(parents=True, exist_ok=True)
    with open(OUTPUT / experiment / 'samples.jsonl', 'w') as output:
        for line in (RAW / experiment / 'samples.jsonl').read_text().splitlines():
            row = json.loads(line)
            row['cpu'] = {name: times for name, times in row['cpu'].items() if name in ('cpu1', 'cpu2', 'cpu3')}
            row.pop('server_io', None)
            output.write(json.dumps(scrub(row), separators=(',', ':')) + '\n')
    environment = json.loads((RAW / experiment / 'environment.json').read_text())
    environment.pop('top', None)
    # The node name, without its domain.
    environment['uname'][1] = environment['uname'][1].split('.')[0]
    (OUTPUT / experiment / 'environment.json').write_text(json.dumps(scrub(environment), indent=1) + '\n')
for cases in ('stall', 'corrupt'):
    (OUTPUT / cases).mkdir(parents=True, exist_ok=True)
    summary = json.loads((RAW / cases / 'summary.json').read_text())
    (OUTPUT / cases / 'summary.json').write_text(json.dumps(scrub(summary), indent=1) + '\n')
(OUTPUT / 'strace').mkdir(parents=True, exist_ok=True)
for name in ('load.txt', 'fixed.txt'):
    (OUTPUT / 'strace' / name).write_text((RAW / 'strace' / name).read_text())
