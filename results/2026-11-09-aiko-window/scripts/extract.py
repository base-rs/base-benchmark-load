#!/usr/bin/env python3
"""Reduce a campaign's samples.jsonl to the committed rows: extract.py SAMPLES LABEL > ROWS.

One row per sample: the driver's window markers, from its record's extension, its operations, and the
server's window CPU time, or, for a failed sample, the last line of the driver's stderr.
"""

import json
import sys

samples, label = sys.argv[1], sys.argv[2]
for line in open(samples):
    s = json.loads(line)
    row = {"campaign": label, "round": s["round"]}
    if s["record"]:
        ext = s["record"]["extensions"]["base-benchmark-load"]
        row["window"] = ext["window"]
        row["operations"] = s["record"]["samples"][0]["operations"]
    else:
        row["error"] = [r.get("stderr", "").strip().splitlines()[-1] for r in s["roles"] if r.get("stderr")]
    roles = [r for r in s["roles"] if r.get("window")]
    for r in roles:
        row["server_window_cpu_ns"] = next(m["measurement"]["value"] for m in r["window"] if m["name"] == "cpu_time")
    print(json.dumps(row, sort_keys=True))
