#!/usr/bin/env python3
"""Pick-off filter (alternative pattern 1): fill-study probes with and without the lag filter.

Usage: scripts/pickoff.py RECORDINGS_DIR [LRE_BINARY]   (writes docs/evidence/pickoff.json)

Sessions B-E, re-quote 1 tick, maker fee 200 ppm, 50 ms latency, proportional queue model;
lag thresholds off, 0, 5 and 20 ticks. Markouts are fill-weighted across sessions.
"""
import json, os, re, subprocess, sys
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REC = os.path.abspath(sys.argv[1])
LRE = sys.argv[2] if len(sys.argv) > 2 else os.path.join(REPO, 'target/release/lre')
SESSIONS = ['session-B', 'session-C', 'session-D', 'session-E']
VARIANTS = [('off', []), ('0', ['0']), ('5', ['5']), ('20', ['20'])]
out = {}
for name, extra in VARIANTS:
    agg = {}
    for s in SESSIONS:
        t = subprocess.run([LRE, 'fill-study', os.path.join(REC, s), '1', '200', '50', *extra], capture_output=True, text=True, check=True).stdout
        for m in re.finditer(r'^study venue=(\w+) model=Queue\(Proportional\) placed=(\d+) fills=(\d+) .*?lag_blocks=(\d+) lag_withdrawals=(\d+)', t, re.M):
            a = agg.setdefault(m[1], dict(placed=0, fills=0, blocks=0, withdrawals=0, h={}))
            a['placed'] += int(m[2]); a['fills'] += int(m[3]); a['blocks'] += int(m[4]); a['withdrawals'] += int(m[5])
        for m in re.finditer(r'^study_markout venue=(\w+) model=Queue\(Proportional\) horizon_ms=(\S+) samples=(\d+) missing=\d+ mean_ticks=(\S+) drift_ticks=(\S+)', t, re.M):
            if m[4] == '-':
                continue
            h = agg[m[1]]['h'].setdefault(m[2], [0.0, 0.0, 0])
            n = int(m[3]); h[0] += float(m[4]) * n; h[1] += float(m[5]) * n; h[2] += n
    for venue, a in agg.items():
        a['markout'] = {h: dict(mean_ticks=v[0] / v[2], drift_ticks=v[1] / v[2], n=v[2]) for h, v in a.pop('h').items() if v[2]}
    out[name] = agg
    print(name, {v: (a['fills'], round(a['markout']['100']['mean_ticks'], 1), round(a['markout']['1000']['mean_ticks'], 1)) for v, a in agg.items()})
json.dump(dict(setup='sessions B-E, re-quote 1, maker fee 200 ppm, 50 ms latency, proportional queue', variants=out),
          open(os.path.join(REPO, 'docs/evidence/pickoff.json'), 'w'), indent=1)
