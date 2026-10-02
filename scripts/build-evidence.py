#!/usr/bin/env python3
"""Rebuild docs/evidence/data.json from the local recordings (never committed). Run
scripts/momentum.py first so its results are included.

Usage: scripts/build-evidence.py RECORDINGS_DIR [LRE_BINARY]

Every number comes from re-running the committed commands on the recordings:
`series`, `cycle-control`, `policy-replay` (variants P1, P2, P3, L3 and L4) and `fill-study`.
Prices are reported relative to each session's research corridor, never as absolute prices.
"""
import hashlib, json, os, re, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REC = os.path.abspath(sys.argv[1])
LRE = sys.argv[2] if len(sys.argv) > 2 else os.path.join(REPO, 'target/release/lre')
SESSIONS = [
    ('A', 'session-A', 'training'),
    ('B', 'session-B', 'training'),
    ('C', 'session-C', 'training'),
    ('D', 'session-D', 'exploratory'),
    ('E', 'session-E', 'pre-registered test'),
]
VENUES = ['binance', 'bybit', 'okx']

def run(*args):
    out = subprocess.run([LRE, *args], capture_output=True, text=True)
    if out.returncode != 0:
        raise SystemExit(f'lre {" ".join(args)} failed: {out.stderr[-500:]}')
    return out.stdout

def cycles(text):
    rows = []
    for m in re.finditer(r'^cycle venue=(\d) side=(\w+) entry=(\d+) exit=(\d+) hold_ms=(\d+) net_atoms=(-?\d+) harvest=(\w+)', text, re.M):
        rows.append(dict(venue=int(m[1]), side=m[2], hold_s=int(m[5]) / 1000, net=int(m[6]), harvest=m[7] == 'true',
                         gross_ticks=(int(m[4]) - int(m[3])) * (1 if m[2] == 'Buy' else -1)))
    return rows

def stats(text, label):
    m = re.search(label + r' cycles=(\d+) .*?mean_net_atoms=(-?[\d.]+) sd=([\d.]+) se=([\d.]+)', text)
    return dict(n=int(m[1]), mean=float(m[2]), sd=float(m[3]), se=float(m[4]))

def series(path):
    lines = run('series', path, '2000').strip().splitlines()[1:]
    out = dict(t=[], mid=[], venues={v: [] for v in VENUES}, env=[], revisits=[], zones=[])
    num = lambda x: float(x) if x else None
    for line in lines:
        t, corridor, mid, b, y, o, env, rev, zones = line.split(',')
        out['t'].append(float(t)); out['mid'].append(num(mid))
        for v, x in zip(VENUES, (b, y, o)):
            out['venues'][v].append(num(x))
        out['env'].append(env); out['revisits'].append(int(rev)); out['zones'].append(int(zones))
    out['corridor'] = int(corridor)
    return out

def study(path):
    text = run('fill-study', path, '1', '200', '50')
    cells = {}
    for m in re.finditer(r'^study venue=(\w+) model=(\S+) placed=(\d+) fills=(\d+) .*?fills_per_min=([\d.]+) fill_ratio=([\d.]+)', text, re.M):
        cells[f'{m[1]}|{m[2]}'] = dict(placed=int(m[3]), fills=int(m[4]), fills_per_min=float(m[5]), fill_ratio=float(m[6]), markout={})
    for m in re.finditer(r'^study_markout venue=(\w+) model=(\S+) horizon_ms=([\d.]+) samples=(\d+) missing=\d+ mean_ticks=(\S+) drift_ticks=(\S+) adverse_fraction=(\S+)', text, re.M):
        if m[5] != '-':
            cells[f'{m[1]}|{m[2]}']['markout'][m[3]] = dict(n=int(m[4]), mean=float(m[5]), drift=float(m[6]), adverse=float(m[7]))
    control = {}
    for m in re.finditer(r'^study_control every_ms=100 horizon_ms=([\d.]+) samples=(\d+) mean_drift_ticks=(\S+) stdev_ticks=(\S+)', text, re.M):
        control[m[1]] = dict(n=int(m[2]), drift=float(m[3]), sd=float(m[4]))
    return dict(cells=cells, control=control)

def session(entry):
    key, name, role = entry
    path = os.path.join(REC, name)
    ctl = run('cycle-control', path, '450', '60', '5', '200')
    variants = {}
    for v in ('p1', 'p2', 'p3', 'l3', 'l4'):
        text = run('policy-replay', path, os.path.join(REPO, f'config/calibration/{v}-structures.toml'),
                   os.path.join(REPO, f'config/calibration/{v}-engine.toml'))
        variants[v] = dict(stats=stats(text, 'engine_cycles'), cycles=cycles(text))
    seconds = float(re.search(r'seconds=([\d.]+)', ctl)[1])
    replayed = int(re.search(r'replayed (\d+) inventory commands; journal verified', run('live-replay', path))[1])
    return key, dict(name=name, role=role, seconds=seconds, replayed=replayed, series=series(path), study=study(path),
                     control=dict(stats=stats(ctl, 'control_cycles all'), cycles=cycles(ctl)), variants=variants)

def manifest():
    rows = {}
    for key, name, _ in SESSIONS:
        d = os.path.join(REC, name)
        for f in sorted(os.listdir(d)):
            if f.endswith('.lre') or f == 'inventory.lri':
                h = hashlib.sha256(open(os.path.join(d, f), 'rb').read()).hexdigest()
                rows[f'{name}/{f}'] = dict(sha256=h, bytes=os.path.getsize(os.path.join(d, f)))
    return rows

with ThreadPoolExecutor(3) as ex:
    sessions = dict(ex.map(session, SESSIONS))
git = subprocess.run(['git', '-C', REPO, 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True).stdout.strip()
momentum_path = os.path.join(REPO, 'docs/evidence/momentum.json')
momentum = json.load(open(momentum_path))['results'] if os.path.exists(momentum_path) else {}
for v in momentum.values():
    v.pop('runs', None)
data = dict(generated_from=git, sessions=sessions, manifest=manifest(), momentum=momentum,
            prereg=dict(commit='94dbd07', results_commit='00cc725'))
os.makedirs(os.path.join(REPO, 'docs/evidence'), exist_ok=True)
json.dump(data, open(os.path.join(REPO, 'docs/evidence/data.json'), 'w'), separators=(',', ':'))
print('wrote docs/evidence/data.json', {k: (v['control']['stats']['n'], v['variants']['p2']['stats']['n']) for k, v in sessions.items()})
