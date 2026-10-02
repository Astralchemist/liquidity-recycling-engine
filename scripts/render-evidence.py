#!/usr/bin/env python3
"""Render docs/evidence/index.html from docs/evidence/data.json (scripts/build-evidence.py).

The page embeds a trimmed copy of the data and loads Chart.js from jsDelivr. Prices appear only
relative to each session's research corridor; P&L is per BTC traded.
"""
import json, math, os, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
data = json.load(open(os.path.join(REPO, 'docs/evidence/data.json')))
S = data['sessions']
KEYS = ['A', 'B', 'C', 'D', 'E']
USDT_PER_ATOM_BTC = 0.01   # net atoms per 0.001 BTC child -> USDT per BTC
USDT_PER_TICK = 0.1        # ticks per unit of price -> USDT per BTC

def st(xs):
    n = len(xs)
    if n == 0:
        return dict(n=0, mean=None, se=None)
    m = sum(xs) / n
    sd = math.sqrt(sum((x - m) ** 2 for x in xs) / (n - 1)) if n > 1 else 0.0
    return dict(n=n, mean=round(m, 2), se=round(sd / math.sqrt(n), 2) if n > 1 else None)

def nets(cycles):
    return [round(c['net'] * USDT_PER_ATOM_BTC, 2) for c in cycles]

hours = sum(S[k]['seconds'] for k in KEYS) / 3600
# Pooled statistics use sessions B-E: session A's 60 s ended with most positions still open.
POOLED = ['B', 'C', 'D', 'E']
pooled_hours = sum(S[k]['seconds'] for k in POOLED) / 3600
by_session = {}
for k in KEYS:
    by_session[k] = dict(control=st(nets(S[k]['control']['cycles'])), p2=st(nets(S[k]['variants']['p2']['cycles'])))

ladder = []
for label, v in [('L0 · spec scopes', 'p1'), ('L1 · venue-local', 'p2'), ('L2 · any calm regime', 'p3'),
                 ('L3 · looser detector', 'l3'), ('L4 · two levels, net 4', 'l4'), ('Ungated dual', 'control')]:
    xs = []
    for k in POOLED:
        xs += nets(S[k]['control']['cycles'] if v == 'control' else S[k]['variants'][v]['cycles'])
    s = st(xs)
    ladder.append(dict(r=label, n=s['n'], perh=round(s['n'] / pooled_hours, 1), mean=s['mean'], se=s['se'],
                       total=round(sum(xs) / 1000, 3)))

cycles = dict(control=[], p2=[], p3=[])
for k in POOLED:
    cycles['control'] += [[k, x] for x in nets(S[k]['control']['cycles'])]
    cycles['p2'] += [[k, x] for x in nets(S[k]['variants']['p2']['cycles'])]

paths = {}
for k in KEYS:
    se = S[k]['series']
    t, m, rev = [], [], []
    last_rev = 0
    for i, (ti, mi, ri) in enumerate(zip(se['t'], se['mid'], se['revisits'])):
        if ri > last_rev:
            rev.append(round(ti, 1))
            last_rev = ri
        if i % 3 == 0 and mi is not None:
            t.append(round(ti, 1))
            m.append(round(mi * USDT_PER_TICK, 2))
    paths[k] = dict(t=t, m=m, rev=rev, corridor=se['corridor'] * USDT_PER_TICK)

markouts = {}
for k in KEYS:
    cells = S[k]['study']['cells']
    row = {}
    for venue in ('binance', 'bybit', 'okx'):
        c = cells.get(f'{venue}|Queue(Proportional)')
        if c:
            row[venue] = [[float(h), round(x['mean'] * USDT_PER_TICK, 2), round(x['drift'] * USDT_PER_TICK, 2), x['n']]
                          for h, x in sorted(c['markout'].items(), key=lambda kv: float(kv[0]))]
            row[venue + '_fills'] = c['fills']
    row['ctl'] = [[float(h), round(x['sd'] * USDT_PER_TICK, 2)] for h, x in sorted(S[k]['study']['control'].items(), key=lambda kv: float(kv[0]))]
    markouts[k] = row

momentum = []
for key, r in data['momentum'].items():
    venue, candle, k = key.split('|')
    if candle not in ('1s', '5s'):
        continue
    f1 = r['forward'].get('1', {}) or {}
    momentum.append(dict(v=venue, c=candle, k=k.replace('sigma', 'σ'), n=r['events'], ph=round(r['per_hour'] or 0, 1),
                         cont=r['continued'], rev=r['reversed'], flat=r['flat'],
                         p=None if r['p_continue'] is None else round(r['p_continue'], 3),
                         b=None if r['base_rate'] is None else round(r['base_rate'], 3),
                         z=None if r['z'] is None else round(r['z'], 2),
                         dur=None if r['mean_duration_s_when_continued'] is None else round(r['mean_duration_s_when_continued'], 1),
                         exc=None if r['mean_excursion_ticks_when_continued'] is None else round(r['mean_excursion_ticks_when_continued'] * USDT_PER_TICK, 1),
                         g1=None if not f1 else round(f1['gross'] * USDT_PER_TICK, 1),
                         net1=None if not f1 else round(f1['net'] * USDT_PER_TICK, 1)))

ledger = []
for k in KEYS:
    files = []
    for f, meta in sorted(data['manifest'].items()):
        if f.startswith(S[k]['name'] + '/'):
            files.append(dict(f=f.split('/', 1)[1], sha=meta['sha256'], mb=round(meta['bytes'] / 1e6, 1)))
    ledger.append(dict(k=k, role=S[k]['role'], s=round(S[k]['seconds'], 1), cmds=S[k]['replayed'], files=files))

def pooled(name):
    xs = [x for _, x in cycles[name]]
    return st(xs)

D = dict(hours=round(hours, 2), bySession=by_session, ladder=ladder, cycles=cycles, paths=paths, markouts=markouts,
         momentum=momentum, ledger=ledger, pooled=dict(control=pooled('control'), p2=pooled('p2')),
         commit=data['generated_from'], prereg=data['prereg'])

E_c, E_p = by_session['E']['control'], by_session['E']['p2']
D_c, D_p = by_session['D']['control'], by_session['D']['p2']
t_E = (E_p['mean'] - E_c['mean']) / math.sqrt(E_p['se'] ** 2 + E_c['se'] ** 2)
t_D = (D_p['mean'] - D_c['mean']) / math.sqrt(D_p['se'] ** 2 + D_c['se'] ** 2)
D['tests'] = dict(tE=round(t_E, 2), tD=round(t_D, 2))

fragment = open(os.path.join(REPO, 'scripts/evidence-template.html')).read()
fragment = fragment.replace('/*__DATA__*/null', json.dumps(D, separators=(',', ':')))
# The repository copy is a complete document (served by GitHub Pages); `--fragment PATH` also
# writes the bare page content, which hosts that add their own document skeleton expect.
document = ('<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
            '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n'
            '<style>html{color-scheme:light dark}body{margin:0}[hidden]{display:none!important}</style>\n'
            + fragment + '\n</html>\n')
out = os.path.join(REPO, 'docs/evidence/index.html')
open(out, 'w').write(document)
if '--fragment' in sys.argv:
    path = sys.argv[sys.argv.index('--fragment') + 1]
    open(path, 'w').write(fragment)
    print('wrote fragment', path)
print('wrote', out, f'{os.path.getsize(out) / 1e3:.0f} kB', 'tE', round(t_E, 2), 'tD', round(t_D, 2))
