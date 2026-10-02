#!/usr/bin/env python3
"""Render the study's figures as SVG (docs/study/figures/) from docs/evidence/*.json.

Usage: scripts/figures.py

Standard library only. Every figure carries its own white background so it reads the same in
GitHub's light and dark themes. Units: USDT per BTC unless stated; a tick is 0.1 USDT.
"""
import json, math, os

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EV = os.path.join(REPO, 'docs/evidence')
OUT = os.path.join(REPO, 'docs/study/figures')
load = lambda n: json.load(open(os.path.join(EV, n)))
data, rob, rep, mom, fund, pick = (load('data.json'), load('robustness.json')['results'], load('replications.json'),
                                   load('momentum.json')['results'], load('funding.json')['pairs'], load('pickoff.json')['variants'])
S = data['sessions']

INK, MUTED, GRID, RULE = '#14202b', '#5a6876', '#e6ebf0', '#cfd7df'
GATED, CONTROL, MOMENTUM, LOSS, GAIN = '#0e6a86', '#7c8b99', '#c27a26', '#b1443a', '#2f7d4f'
VENUE = {'binance': GATED, 'bybit': MOMENTUM, 'okx': LOSS}
NAME = {'binance': 'Binance', 'bybit': 'Bybit', 'okx': 'OKX'}
FONT = "font-family=\"'IBM Plex Sans', 'Helvetica Neue', Arial, sans-serif\""
MONO = "font-family=\"'IBM Plex Mono', Menlo, monospace\""

def esc(s):
    return str(s).replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')

def nice_ticks(lo, hi, n=5):
    span = hi - lo
    if span <= 0:
        return [lo]
    raw = span / n
    mag = 10 ** math.floor(math.log10(raw))
    step = min((m * mag for m in (1, 2, 2.5, 5, 10) if m * mag >= raw), default=10 * mag)
    first = math.ceil(lo / step) * step
    out, v = [], first
    while v <= hi + 1e-9:
        out.append(round(v, 10))
        v += step
    return out

def fmt(v):
    if abs(v) >= 1000:
        return f'{v:,.0f}'
    if abs(v - round(v)) < 1e-9:
        return f'{int(round(v))}'
    return f'{v:.1f}' if abs(v) >= 1 else f'{v:.2g}'

class Panel:
    """One plotting area at (x, y) of size (w, h) inside a figure, with data ranges."""
    def __init__(self, fig, x, y, w, h, xr, yr, xlog=False, ylog=False, ml=58, mr=14, mt=30, mb=44):
        self.f, self.x0, self.y0 = fig, x + ml, y + mt
        self.w, self.h = w - ml - mr, h - mt - mb
        self.xr, self.yr, self.xlog, self.ylog = xr, yr, xlog, ylog
        self.box = (x, y, w, h)

    def px(self, v):
        a, b = self.xr
        if self.xlog:
            v, a, b = math.log10(v), math.log10(a), math.log10(b)
        return self.x0 + (v - a) / (b - a) * self.w

    def py(self, v):
        a, b = self.yr
        if self.ylog:
            v, a, b = math.log10(max(v, 1e-12)), math.log10(a), math.log10(b)
        return self.y0 + self.h - (v - a) / (b - a) * self.h

    def frame(self, title=None, xlabel=None, ylabel=None, xticks=None, yticks=None, xtick_labels=None, grid=True):
        f = self.f
        if title:
            f.text(self.box[0] + 4, self.box[1] + 16, title, size=13, weight=600)
        yt = yticks if yticks is not None else (self.logticks(self.yr) if self.ylog else nice_ticks(*self.yr))
        for v in yt:
            y = self.py(v)
            if grid:
                f.line(self.x0, y, self.x0 + self.w, y, GRID, 1)
            f.text(self.x0 - 6, y + 4, fmt(v), size=10.5, anchor='end', color=MUTED, mono=True)
        if xtick_labels:
            for v, label in xtick_labels:
                for row, part in enumerate(label.split('\n')):
                    f.text(self.px(v), self.y0 + self.h + 15 + 13 * row, part, size=10.5, anchor='middle', color=INK if row == 0 else MUTED)
        else:
            xt = xticks if xticks is not None else (self.logticks(self.xr) if self.xlog else nice_ticks(*self.xr))
            for v in xt:
                x = self.px(v)
                f.line(x, self.y0 + self.h, x, self.y0 + self.h + 4, RULE, 1)
                f.text(x, self.y0 + self.h + 16, fmt(v), size=10.5, anchor='middle', color=MUTED, mono=True)
        f.line(self.x0, self.y0 + self.h, self.x0 + self.w, self.y0 + self.h, RULE, 1)
        if xlabel:
            f.text(self.x0 + self.w / 2, self.y0 + self.h + 34, xlabel, size=11, anchor='middle', color=MUTED)
        if ylabel:
            f.out.append(f'<text x="{self.box[0] + 12}" y="{self.y0 + self.h / 2}" transform="rotate(-90 {self.box[0] + 12} {self.y0 + self.h / 2})" '
                         f'text-anchor="middle" font-size="11" fill="{MUTED}" {FONT}>{esc(ylabel)}</text>')

    @staticmethod
    def logticks(r):
        a, b = math.floor(math.log10(r[0])), math.ceil(math.log10(r[1]))
        return [10 ** k for k in range(a, b + 1) if r[0] <= 10 ** k <= r[1]]

    def hline(self, v, color, dash=None, label=None, width=1.2, left=False):
        y = self.py(v)
        self.f.line(self.x0, y, self.x0 + self.w, y, color, width, dash)
        if label:
            x, anchor = (self.x0 + 4, 'start') if left else (self.x0 + self.w - 4, 'end')
            self.f.text(x, y - 5, label, size=10, anchor=anchor, color=color)

    def vline(self, v, color, dash=None, label=None):
        x = self.px(v)
        self.f.line(x, self.y0, x, self.y0 + self.h, color, 1.2, dash)
        if label:
            self.f.text(x + 4, self.y0 + 11, label, size=10, color=color)

    def bar(self, xc, width_px, v, color, base=0.0):
        y0, y1 = self.py(base), self.py(v)
        self.f.rect(xc - width_px / 2, min(y0, y1), width_px, abs(y1 - y0), color)

    def whisker(self, xc, lo, hi, color=INK):
        a, b = self.py(lo), self.py(hi)
        self.f.line(xc, a, xc, b, color, 1.4)
        self.f.line(xc - 4, a, xc + 4, a, color, 1.4)
        self.f.line(xc - 4, b, xc + 4, b, color, 1.4)

    def path(self, pts, color, width=1.6, dash=None):
        pts = [(x, y) for x, y in pts if x is not None and y is not None]
        if len(pts) < 2:
            return
        d = 'M' + ' L'.join(f'{self.px(x):.1f},{self.py(y):.1f}' for x, y in pts)
        da = f' stroke-dasharray="{dash}"' if dash else ''
        self.f.out.append(f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{width}"{da} stroke-linejoin="round"/>')

    def dot(self, x, y, color, r=3.0, opacity=1.0):
        self.f.out.append(f'<circle cx="{self.px(x):.1f}" cy="{self.py(y):.1f}" r="{r}" fill="{color}" fill-opacity="{opacity}"/>')

    def band(self, lo, hi, color, opacity=0.09):
        y0, y1 = self.py(hi), self.py(lo)
        self.f.out.append(f'<rect x="{self.x0}" y="{y0:.1f}" width="{self.w}" height="{y1 - y0:.1f}" fill="{color}" fill-opacity="{opacity}"/>')

class Figure:
    def __init__(self, w, h, title, subtitle=None):
        self.w, self.h, self.out = w, h, []
        self.out.append(f'<rect width="{w}" height="{h}" fill="#ffffff"/>')
        self.text(20, 28, title, size=16, weight=700)
        if subtitle:
            self.text(20, 47, subtitle, size=11.5, color=MUTED)

    def text(self, x, y, s, size=12, anchor='start', color=INK, weight=400, mono=False):
        self.out.append(f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" text-anchor="{anchor}" fill="{color}" font-weight="{weight}" '
                        f'{MONO if mono else FONT}>{esc(s)}</text>')

    def line(self, x1, y1, x2, y2, color, width=1.0, dash=None):
        da = f' stroke-dasharray="{dash}"' if dash else ''
        self.out.append(f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" stroke="{color}" stroke-width="{width}"{da}/>')

    def rect(self, x, y, w, h, color, opacity=1.0):
        self.out.append(f'<rect x="{x:.1f}" y="{y:.1f}" width="{max(w, 0):.1f}" height="{max(h, 0):.1f}" fill="{color}" fill-opacity="{opacity}"/>')

    def legend(self, x, y, items):
        cx = x
        for label, color, kind in items:
            if kind == 'line':
                self.line(cx, y - 4, cx + 16, y - 4, color, 2.2)
            elif kind == 'dash':
                self.line(cx, y - 4, cx + 16, y - 4, color, 1.6, '5 4')
            else:
                self.rect(cx, y - 10, 12, 12, color)
            self.text(cx + 20, y, label, size=11, color=INK)
            cx += 26 + 6.4 * len(label)

    def save(self, name):
        os.makedirs(OUT, exist_ok=True)
        svg = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w}" height="{self.h}" viewBox="0 0 {self.w} {self.h}" role="img">'
               + ''.join(self.out) + '</svg>\n')
        open(os.path.join(OUT, name), 'w').write(svg)
        print('wrote', name)

def nets(cycles):
    return [c['net'] * 0.01 for c in cycles]

# Figure 1: the pre-registered test.
def fig1():
    f = Figure(900, 452, 'Figure 1. Pre-registered test: does the void-revisit gate pick better round trips?',
               'Mean net result per round trip after retail fees, USDT per BTC, with 95% block-bootstrap intervals. Session E is the holdout.')
    keys = ['B', 'C', 'D', 'E', 'pooled_BCDE']
    labels = ['Session B', 'Session C', 'Session D', 'Session E (test)', 'Pooled B–E']
    lo = min(min(r['control_ci'][0], (r.get('gated_ci') or [0, 0])[0]) for r in rob.values())
    p = Panel(f, 10, 82, 880, 340, (-0.5, 4.5), (min(-120, lo - 10), 30))
    p.frame(ylabel='USDT per BTC per round trip', xtick_labels=list(enumerate(labels)))
    p.hline(0, MUTED)
    for i, k in enumerate(keys):
        r = rob[k]
        x = p.px(i)
        p.bar(x - 22, 40, r['control_mean'], CONTROL)
        p.whisker(x - 22, *r['control_ci'])
        if r.get('gated_n'):
            p.bar(x + 22, 40, r['gated_mean'], GATED)
            p.whisker(x + 22, *r['gated_ci'])
            f.text(x + 22, p.py(r['gated_ci'][0]) + 14, f"n={r['gated_n']}", size=10, anchor='middle', color=GATED, mono=True)
        else:
            f.text(x + 22, p.py(0) + 14, 'no gated', size=10, anchor='middle', color=MUTED)
            f.text(x + 22, p.py(0) + 26, 'trips', size=10, anchor='middle', color=MUTED)
    f.legend(560, 70, [('Ungated control', CONTROL, 'box'), ('Void-revisit gate (P2)', GATED, 'box')])
    f.save('fig01-preregistered-test.svg')

# Figure 2: every round trip.
def fig2():
    f = Figure(900, 322, 'Figure 2. Every round trip, sessions B–E',
               'One dot per 0.001 BTC round trip, scaled to USDT per BTC. Bars mark the means; dashed: round-trip maker fees.')
    ctrl = [x for k in 'BCDE' for x in nets(S[k]['control']['cycles'])]
    gate = [x for k in 'BCDE' for x in nets(S[k]['variants']['p2']['cycles'])]
    lo = math.floor(min(ctrl + gate) / 50) * 50
    p = Panel(f, 10, 78, 880, 230, (lo, 30), (-0.7, 1.7), ml=150)
    p.frame(xlabel='Net per round trip, USDT per BTC', yticks=[], grid=False)
    for i, (name, xs, color, r) in enumerate([(f'Control ({len(ctrl)})', ctrl, CONTROL, 2.2), (f'Gated ({len(gate)})', gate, GATED, 4)]):
        f.text(p.x0 - 10, p.py(i) + 4, name, size=11.5, anchor='end')
        seed = 7 + i
        for x in xs:
            seed = (seed * 16807) % 2147483647
            p.dot(x, i + (seed / 2147483647 - 0.5) * 0.5, color, r, 0.35 if i == 0 else 0.9)
        m = sum(xs) / len(xs)
        f.line(p.px(m), p.py(i) - 18, p.px(m), p.py(i) + 18, INK, 3)
        # Control's label below its row, the gated one above, so close means never collide.
        f.text(p.px(m) - 6, p.py(i) + (32 if i == 0 else -24), f'mean {m:+.1f}', size=10, anchor='end', mono=True)
    p.vline(0, MUTED)
    p.vline(-34.6, LOSS, '5 4', 'maker fees')
    f.save('fig02-round-trips.svg')

# Figure 3: the loosening ladder.
def fig3():
    f = Figure(900, 382, 'Figure 3. Loosening the two-sided entry multiplies trades, not results',
               'Sessions B–E. Left: round trips per hour (log scale). Right: mean net per round trip, ±1 standard error.')
    rungs = [('L0', 'p1'), ('L1', 'p2'), ('L2', 'p3'), ('L3', 'l3'), ('L4', 'l4'), ('Dual', 'control')]
    hours = sum(S[k]['seconds'] for k in 'BCDE') / 3600
    stats = []
    for label, v in rungs:
        xs = [x for k in 'BCDE' for x in nets(S[k]['control']['cycles'] if v == 'control' else S[k]['variants'][v]['cycles'])]
        n = len(xs); m = sum(xs) / n
        se = math.sqrt(sum((x - m) ** 2 for x in xs) / (n - 1)) / math.sqrt(n) if n > 1 else 0
        stats.append((label, n / hours, m, se))
    a = Panel(f, 10, 78, 440, 290, (-0.5, 5.5), (0.5, 1000), ylog=True)
    a.frame(ylabel='Round trips per hour', xtick_labels=[(i, s[0]) for i, s in enumerate(stats)])
    for i, (label, rate, m, se) in enumerate(stats):
        a.bar(a.px(i), 34, rate, CONTROL if label == 'Dual' else GATED, base=0.5)
        f.text(a.px(i), a.py(rate) - 5, f'{rate:.1f}', size=10, anchor='middle', mono=True)
    b = Panel(f, 450, 78, 440, 290, (-0.5, 5.5), (-60, 15))
    b.frame(ylabel='USDT per BTC per round trip', xtick_labels=[(i, s[0]) for i, s in enumerate(stats)])
    b.hline(0, MUTED)
    for i, (label, rate, m, se) in enumerate(stats):
        b.bar(b.px(i), 34, m, GAIN if m >= 0 else LOSS)
        if se:
            b.whisker(b.px(i), m - se, m + se)
    f.save('fig03-loosening-ladder.svg')

# Figure 4: session E within the research corridor.
def fig4():
    se = S['E']['series']
    f = Figure(900, 382, 'Figure 4. Session E: the price left the fixed research corridor after about 12 minutes',
               'Composite midpoint relative to the corridor floor (USDT). Shaded: the 102.4 USDT corridor fixed at session start. Dots: observed void revisits.')
    t = [x / 60 for x in se['t']]
    m = [None if v is None else v * 0.1 for v in se['mid']]
    lo = min(v for v in m if v is not None)
    p = Panel(f, 10, 82, 880, 290, (0, t[-1]), (math.floor(lo / 50) * 50, 130))
    p.frame(xlabel='Minutes into the session', ylabel='USDT above the corridor floor')
    p.band(0, se['corridor'] * 0.1, GATED, 0.12)
    p.hline(0, GATED, '4 4')
    p.hline(se['corridor'] * 0.1, GATED, '4 4', 'corridor top')
    p.path(list(zip(t, m)), INK, 1.2)
    last = 0
    for ti, mi, r in zip(t, m, se['revisits']):
        if r > last and mi is not None:
            p.dot(ti, mi, MOMENTUM, 4)
            last = r
    out = sum(1 for v in m if v is not None and (v < 0 or v > se['corridor'] * 0.1)) / len([v for v in m if v is not None])
    f.text(880, 367, f'price outside the corridor {100 * out:.0f}% of the hour', size=11, anchor='end', color=MUTED)
    f.save('fig04-corridor.svg')

# Figure 5: fill-study markouts.
def fig5():
    cells, ctl = S['E']['study']['cells'], S['E']['study']['control']
    f = Figure(900, 402, 'Figure 5. Passive fills at the touch are adversely selected',
               'Session E, proportional queue model, 50 ms latency. Mean markout per BTC after a fill; dashed grey: unconditional σ of midpoint moves.')
    p = Panel(f, 10, 82, 880, 300, (1, 5000), (-25, 30), xlog=True)
    p.frame(xlabel='Horizon after the fill, ms (log)', ylabel='USDT per BTC', xticks=[1, 10, 100, 1000, 5000])
    p.hline(0, MUTED)
    p.hline(-17.3, LOSS, '5 4', 'maker fee per fill (retail)')
    for venue, color in VENUE.items():
        c = cells[f'{venue}|Queue(Proportional)']
        pts = sorted((float(h), x['mean'] * 0.1) for h, x in c['markout'].items())
        p.path(pts, color, 2)
        for x, y in pts:
            p.dot(x, y, color, 2.6)
    p.path(sorted((float(h), x['sd'] * 0.1) for h, x in ctl.items()), CONTROL, 1.6, '5 4')
    f.legend(560, 70, [('Binance', VENUE['binance'], 'line'), ('Bybit', VENUE['bybit'], 'line'), ('OKX', VENUE['okx'], 'line')])
    f.save('fig05-markouts.svg')

# Figure 6: the pick-off filter.
def fig6():
    f = Figure(900, 402, 'Figure 6. Pick-off filter: not quoting a venue\'s stale side halves adverse selection',
               'Sessions B–E, 50 ms latency. Fill-weighted mean markout per BTC. Threshold θ: venue midpoint versus composite, in ticks.')
    variants = [('off', 'no filter', CONTROL), ('20', 'θ = 20', '#9fc3cf'), ('5', 'θ = 5', '#4f97ad'), ('0', 'θ = 0', GATED)]
    venues = ['binance', 'bybit', 'okx']
    for pi, (h, title) in enumerate([('100', 'Markout at 100 ms'), ('1000', 'Markout at 1 s')]):
        p = Panel(f, 10 + pi * 440, 82, 440, 300, (-0.5, 2.5), (-8, 2) if h == '100' else (-8, 4))
        p.frame(title=title, ylabel='USDT per BTC', xtick_labels=[(i, NAME[v]) for i, v in enumerate(venues)])
        p.hline(0, MUTED)
        if h == '1000':
            p.hline(3.46, GAIN, '5 4', 'Bybit MM3 rebate (40 ppm)')
        for i, venue in enumerate(venues):
            for j, (key, label, color) in enumerate(variants):
                v = pick[key][venue]['markout'][h]['mean_ticks'] * 0.1
                p.bar(p.px(i) + (j - 1.5) * 22, 19, v, color)
    f.legend(330, 70, [(label, color, 'box') for _, label, color in variants])
    f.save('fig06-pickoff-filter.svg')

# Figure 7: Cont, Kukanov & Stoikov replication.
def fig7():
    f = Figure(960, 422, 'Figure 7. Replicating Cont, Kukanov & Stoikov (2014): price changes are linear in order-flow imbalance',
               'Left: mean price change per OFI bin, 10 s intervals. Middle: R² of OFI versus trade imbalance. Right: impact coefficient versus depth, 1 s intervals, 5-minute blocks.')
    c10, c1 = rep['cks']['10s'], rep['cks']['1s']
    allx = [b[0] for v in c10.values() for b in v['bins']]
    ally = [b[1] for v in c10.values() for b in v['bins']]
    a = Panel(f, 10, 82, 320, 320, (min(allx) * 1.05, max(allx) * 1.05), (min(ally) * 1.15, max(ally) * 1.15))
    a.frame(title='Price change vs OFI (10 s)', xlabel='OFI, BTC', ylabel='Mean price change, ticks')
    for venue, color in VENUE.items():
        for x, y in c10[venue]['bins']:
            a.dot(x, y, color, 3.2)
        b, al = c10[venue]['beta_ticks_per_btc'], c10[venue]['alpha']
        a.path([(a.xr[0], al + b * a.xr[0]), (a.xr[1], al + b * a.xr[1])], color, 1.2, '4 3')
    b = Panel(f, 330, 82, 300, 320, (-0.5, 5.5), (0, 1))
    labels = [(i, s) for i, s in enumerate(['Binance\n1 s', 'Bybit\n1 s', 'OKX\n1 s', 'Binance\n10 s', 'Bybit\n10 s', 'OKX\n10 s'])]
    b.frame(title='Variance explained', ylabel='R²', xtick_labels=labels)
    for i, (src, venue) in enumerate([(c1, 'binance'), (c1, 'bybit'), (c1, 'okx'), (c10, 'binance'), (c10, 'bybit'), (c10, 'okx')]):
        b.bar(b.px(i) - 9, 16, src[venue]['r2_ofi'], GATED)
        b.bar(b.px(i) + 9, 16, src[venue]['r2_ti'], CONTROL)
    pts = [(x, y, venue) for venue in VENUE for x, y in c1[venue]['depth']['points']]
    xs, ys = [p_[0] for p_ in pts], [p_[1] for p_ in pts]
    c = Panel(f, 630, 82, 320, 320, (min(xs) - 0.1, max(xs) + 0.1), (min(ys) - 0.2, max(ys) + 0.2))
    c.frame(title='log β vs log depth (1 s)', xlabel='log mean touch depth, BTC', ylabel='log β')
    for x, y, venue in pts:
        c.dot(x, y, VENUE[venue], 2.6, 0.75)
    for venue, color in VENUE.items():
        d = c1[venue]['depth']
        vx = [p_[0] for p_ in d['points']]
        c.path([(min(vx), d['intercept'] + d['slope'] * min(vx)), (max(vx), d['intercept'] + d['slope'] * max(vx))], color, 2)
    mx, my = sum(xs) / len(xs), sum(ys) / len(ys)
    c.path([(c.xr[0], my - (c.xr[0] - mx)), (c.xr[1], my - (c.xr[1] - mx))], INK, 1.4, '5 4')
    slopes = ', '.join(f"{NAME[v]} {c1[v]['depth']['slope']:.2f}" for v in VENUE)
    f.text(940, 415, f'solid: per-venue fits ({slopes}); dashed: slope −1, the CKS prediction', size=10.5, anchor='end', color=MUTED)
    f.legend(330, 413, [('OFI', GATED, 'box'), ('Trade imbalance', CONTROL, 'box')])
    f.legend(20, 413, [('Binance', VENUE['binance'], 'box'), ('Bybit', VENUE['bybit'], 'box'), ('OKX', VENUE['okx'], 'box')])
    f.save('fig07-cks-replication.svg')

# Figure 8: Farmer et al. replication.
def fig8():
    fi, fo = rep['farmer_intervals'], rep['farmer']
    f = Figure(960, 402, 'Figure 8. Testing Farmer et al. (2004): what drives the largest price changes?',
               'One-second intervals and individual market orders, sessions B–E. Book flow (OFI) explains moves better than traded volume.')
    venues = ['binance', 'bybit', 'okx']
    a = Panel(f, 10, 82, 320, 300, (-0.5, 2.5), (0, 0.6))
    a.frame(title='R² of |price change|', ylabel='R²', xtick_labels=[(i, NAME[v]) for i, v in enumerate(venues)])
    for i, v in enumerate(venues):
        a.bar(a.px(i) - 13, 22, fi[v]['r2_abs_move_on_abs_ofi'], GATED)
        a.bar(a.px(i) + 13, 22, fi[v]['r2_abs_move_on_abs_ti'], CONTROL)
    b = Panel(f, 330, 82, 310, 300, (-0.5, 2.5), (0.01, 10), ylog=True)
    b.frame(title='Traded volume ÷ touch depth', ylabel='Median ratio (log)', xtick_labels=[(i, NAME[v]) for i, v in enumerate(venues)])
    b.hline(1, MUTED, '4 4', 'one touch')
    for i, v in enumerate(venues):
        b.bar(b.px(i) - 13, 22, fi[v]['median_large_volume_over_touch'], MOMENTUM, base=0.01)
        b.bar(b.px(i) + 13, 22, fi[v]['median_all_volume_over_touch'], CONTROL, base=0.01)
    c = Panel(f, 640, 82, 310, 300, (-0.5, 2.5), (0, 1))
    c.frame(title='Largest 1% of order-level moves', ylabel='Share', xtick_labels=[(i, NAME[v]) for i, v in enumerate(venues)])
    for i, v in enumerate(venues):
        share = fo[v]['large']['share_smaller_than_best']
        c.bar(c.px(i), 30, share, GATED)
        f.text(c.px(i), c.py(share) - 5, f'{100 * share:.0f}%', size=10, anchor='middle', mono=True)
    f.text(940, 395, 'right: share where the order was smaller than the best level it hit', size=10.5, anchor='end', color=MUTED)
    f.legend(20, 395, [('OFI', GATED, 'box'), ('Traded volume', CONTROL, 'box'), ('Largest 1% of 1 s moves', MOMENTUM, 'box')])
    f.save('fig08-farmer-replication.svg')

# Figure 9: momentum after pumps.
def fig9():
    rows = [(k, v) for k, v in mom.items() if k.split('|')[1] in ('1s', '5s') and not k.endswith('5s|5sigma')]
    f = Figure(960, 382, 'Figure 9. After a mega pump the next candle continues no more often than any candle',
               'Share of next candles moving in the pump\'s direction (amber) versus the base rate for every pair of candles (grey).')
    p = Panel(f, 10, 82, 940, 280, (-0.5, len(rows) - 0.5), (0, 1))
    labels = []
    for i, (k, v) in enumerate(rows):
        venue, candle, sig = k.split('|')
        labels.append((i, f"{NAME[venue]} {candle} ≥{sig[0]}σ"))
    p.frame(ylabel='Share of candles', xtick_labels=labels)
    p.hline(0.5, MUTED, '3 4', 'coin flip', left=True)
    for i, (k, v) in enumerate(rows):
        if v['p_continue'] is not None:
            p.bar(p.px(i) - 11, 20, v['p_continue'], MOMENTUM)
            f.text(p.px(i) - 11, p.py(v['p_continue']) - 5, f"n={v['events']}", size=9.5, anchor='middle', mono=True)
        p.bar(p.px(i) + 11, 20, v['base_rate'], CONTROL)
    f.save('fig09-momentum.svg')

# Figure 10: cross-venue gaps.
def fig10():
    d = rep['divergence']
    f = Figure(900, 382, 'Figure 10. Cross-venue price gaps almost never exceed the cost of trading them',
               'Distribution of |midpoint gap| between venues, 200 ms samples, sessions B–E (log count). Dashed: two-venue round-trip costs.')
    hmax = max(max(v['hist']) for v in d['pairs'].values())
    p = Panel(f, 10, 82, 880, 280, (0, 180), (0.5, hmax * 1.5), ylog=True)
    p.frame(xlabel='|gap|, USDT per BTC', ylabel='Samples (log)')
    colors = {'binance-bybit': GATED, 'binance-okx': LOSS, 'bybit-okx': MOMENTUM}
    for pair, v in d['pairs'].items():
        pts = [((e + 12.5) * 0.1, max(c, 0.5)) for e, c in zip(v['hist_edges_ticks'][:-1], v['hist'])]
        p.path(pts, colors[pair], 1.8)
    p.vline(d['hurdles']['maker_two_venue_ticks'] * 0.1, LOSS, '5 4', 'maker fees, 4 fills')
    p.vline(d['hurdles']['taker_two_venue_ticks'] * 0.1 if d['hurdles']['taker_two_venue_ticks'] * 0.1 < 180 else 178, LOSS, '2 3', 'taker fees: 173 →')
    f.legend(560, 70, [('–'.join(NAME[x] for x in k.split('-')), c, 'line') for k, c in colors.items()])
    f.save('fig10-cross-venue-gaps.svg')

# Figure 11: funding carry.
def fig11():
    v = fund['binance-bybit']
    f = Figure(900, 382, 'Figure 11. Cross-venue funding carry on BTC: the spread is too small to pay for trading it',
               f'Binance vs Bybit, {v["first"]} to {v["last"]}, cumulative return on one leg, basis points.')
    series = [('Hindsight, always on the right side (gross)', v['hindsight_cumulative_bps'], GAIN, None),
              ('Rule, 1 bp hurdle, maker fees', v['strategies']['maker|1bps']['cumulative_bps'], GATED, None),
              ('Rule, no hurdle, maker fees', v['strategies']['maker|0bps']['cumulative_bps'], LOSS, None)]
    lo = min(min(s[1]) for s in series); hi = max(max(s[1]) for s in series)
    n = max(len(s[1]) for s in series)
    p = Panel(f, 10, 82, 880, 280, (0, n), (lo * 1.1 if lo < 0 else -5, hi * 1.2 + 5))
    p.frame(xlabel='Days', ylabel='Cumulative, bp of one leg')
    p.hline(0, MUTED)
    for label, ys, color, dash in series:
        p.path(list(enumerate(ys)), color, 2, dash)
    f.legend(20, 70, [(s[0], s[2], 'line') for s in series])
    f.save('fig11-funding-carry.svg')

for fn in (fig1, fig2, fig3, fig4, fig5, fig6, fig7, fig8, fig9, fig10, fig11):
    fn()
