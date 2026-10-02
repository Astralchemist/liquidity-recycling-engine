#!/usr/bin/env python3
"""Bootstrap confidence intervals for the calibration comparisons (docs/calibration.md).

Usage: scripts/robustness.py   (reads docs/evidence/data.json, writes docs/evidence/robustness.json)

The control's round trips share one price path, so they are serially correlated. (In practice
consecutive trips are NEGATIVELY correlated, long and short cyclers offsetting, so the plain
standard error turned out conservative, not optimistic.) Control means use a moving-block
bootstrap (Kunsch 1989) over the time-ordered trips with block length 20; the gated strategy's
trips come from separate episodes and are resampled individually. 10,000 resamples, fixed seed;
95% percentile intervals.
"""
import json, os, random

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
data = json.load(open(os.path.join(REPO, 'docs/evidence/data.json')))['sessions']
rng = random.Random(20261002)
B, BLOCK = 10_000, 20

def nets(cycles):
    return [c['net'] * 0.01 for c in cycles]  # atoms per 0.001 BTC -> USDT per BTC

def block_means(xs):
    n = len(xs)
    if n == 0:
        return []
    if n <= BLOCK:
        return [sum(rng.choice(xs) for _ in range(n)) / n for _ in range(B)]
    starts = range(n - BLOCK + 1)
    out = []
    for _ in range(B):
        sample = []
        while len(sample) < n:
            s = rng.choice(starts)
            sample += xs[s:s + BLOCK]
        out.append(sum(sample[:n]) / n)
    return out

def iid_means(xs):
    n = len(xs)
    return [sum(rng.choice(xs) for _ in range(n)) / n for _ in range(B)] if n else []

def ci(vals):
    s = sorted(vals)
    return [s[int(0.025 * (len(s) - 1))], s[int(0.975 * (len(s) - 1))]]

def compare(control, gated):
    c, g = block_means(control), iid_means(gated)
    d = [x - y for x, y in zip(g, c)]
    return dict(control_mean=sum(control) / len(control), control_ci=ci(c),
                gated_n=len(gated), gated_mean=sum(gated) / len(gated) if gated else None, gated_ci=ci(g) if g else None,
                difference=(sum(gated) / len(gated) - sum(control) / len(control)) if gated else None,
                difference_ci=ci(d) if g else None,
                p_difference_above_zero=sum(1 for x in d if x > 0) / len(d) if d else None)

out = {}
for key in ('B', 'C'):
    c = nets(data[key]['control']['cycles'])
    out[key] = dict(control_mean=sum(c) / len(c), control_ci=ci(block_means(c)), gated_n=0)
for key in ('D', 'E'):
    out[key] = compare(nets(data[key]['control']['cycles']), nets(data[key]['variants']['p2']['cycles']))
pooled_c = [x for k in 'BCDE' for x in nets(data[k]['control']['cycles'])]
pooled_g = [x for k in 'BCDE' for x in nets(data[k]['variants']['p2']['cycles'])]
out['pooled_BCDE'] = compare(pooled_c, pooled_g)
for k, v in out.items():
    if not v['gated_n']:
        print(f"{k}: control {v['control_mean']:.1f} [{v['control_ci'][0]:.1f}, {v['control_ci'][1]:.1f}]  (no gated trips)")
        continue
    print(f"{k}: control {v['control_mean']:.1f} [{v['control_ci'][0]:.1f}, {v['control_ci'][1]:.1f}]  "
          f"gated(n={v['gated_n']}) {v['gated_mean']:.1f} [{v['gated_ci'][0]:.1f}, {v['gated_ci'][1]:.1f}]  "
          f"difference {v['difference']:+.1f} [{v['difference_ci'][0]:+.1f}, {v['difference_ci'][1]:+.1f}]  P(>0)={v['p_difference_above_zero']:.3f}  (USDT per BTC)")
json.dump(dict(method='moving-block bootstrap (control, block 20) and iid bootstrap (gated), 10,000 resamples, seed 20261002', results=out),
          open(os.path.join(REPO, 'docs/evidence/robustness.json'), 'w'), indent=1)
