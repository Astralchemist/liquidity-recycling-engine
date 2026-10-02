#!/usr/bin/env python3
"""Replications of two published order-book results on our recordings, plus the cross-venue
gap test. Standard library only. Writes docs/evidence/replications.json.

Usage: scripts/replications.py RECORDINGS_DIR [LRE_BINARY]

1. Cont, Kukanov & Stoikov (2014; arXiv 2010), "The price impact of order book events":
   - Contemporaneous: dP_k = a + b * OFI_k over intervals of 1 s and 10 s, with OFI from the
     engine's production best-quote implementation (`lre ofi-series`), versus the same
     regression on trade imbalance TI_k. CKS report OFI explaining most price variance and TI
     much less.
   - Depth: per 5-minute block i, b_i against mean touch depth D_i; CKS report b_i ~ c / D_i,
     i.e. a slope near -1 in log b on log D.
   - Predictive (our addition, the trading question): dP_{k+1} on OFI_k, and whether the move
     after the strongest OFI decile could pay taker fees.
2. Farmer, Gillemot, Lillo, Mike & Sen (2004), "What really causes large price changes?":
   large price changes are driven by gaps in the order book (liquidity fluctuations), not by
   large orders. Order level (`lre impact-events`): for orders that clear the best level, how
   much of the price change is explained by the first gap versus by order size; for the
   largest 1% of price changes, how large were the orders and the gaps.
3. Cross-venue gaps (alternative pattern): how often venue midpoints diverge by more than the
   round-trip cost of a two-venue convergence trade.

Statistics: OLS with Newey-West (Bartlett) standard errors, because consecutive intervals are
serially correlated; ticks are 0.1 USDT, quantity units 0.0001 BTC.
"""
import json, math, os, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REC = os.path.abspath(sys.argv[1])
LRE = sys.argv[2] if len(sys.argv) > 2 else os.path.join(REPO, 'target/release/lre')
SESSIONS = ['session-B', 'session-C', 'session-D', 'session-E']
VENUES = {1: 'binance', 2: 'bybit', 3: 'okx'}
UNITS_PER_BTC = 10_000
TAKER_ROUND_TRIP_TICKS = 866  # 2 x 0.05% of ~86,600 USDT, in 0.1 USDT ticks
MAKER_TWO_VENUE_TICKS = 692   # 4 maker fills x 0.02%
TAKER_TWO_VENUE_TICKS = 1732  # 4 taker fills x 0.05%

def lre(*args):
    out = subprocess.run([LRE, *args], capture_output=True, text=True)
    if out.returncode != 0:
        raise SystemExit(f'lre {" ".join(args)}: {out.stderr[-400:]}')
    return out.stdout.strip().splitlines()

def ols(x_cols, y, lags):
    """OLS with intercept; returns coefficients, Newey-West SEs and R^2."""
    n, k = len(y), len(x_cols) + 1
    X = [[1.0] + [c[i] for c in x_cols] for i in range(n)]
    xtx = [[sum(X[r][a] * X[r][b] for r in range(n)) for b in range(k)] for a in range(k)]
    xty = [sum(X[r][a] * y[r] for r in range(n)) for a in range(k)]
    inv = invert(xtx)
    beta = [sum(inv[a][b] * xty[b] for b in range(k)) for a in range(k)]
    res = [y[r] - sum(beta[a] * X[r][a] for a in range(k)) for r in range(n)]
    ybar = sum(y) / n
    ss_tot = sum((v - ybar) ** 2 for v in y)
    r2 = 1 - sum(e * e for e in res) / ss_tot if ss_tot else 0.0
    # Newey-West meat: sum over lags of Bartlett-weighted autocovariances of X_r * e_r.
    g = [[X[r][a] * res[r] for a in range(k)] for r in range(n)]
    meat = [[0.0] * k for _ in range(k)]
    for lag in range(lags + 1):
        w = 1.0 if lag == 0 else 1 - lag / (lags + 1)
        for r in range(lag, n):
            for a in range(k):
                for b in range(k):
                    v = g[r][a] * g[r - lag][b]
                    meat[a][b] += w * v if lag == 0 else w * (v + g[r - lag][a] * g[r][b])
    cov = mat(mat(inv, meat), inv)
    se = [math.sqrt(max(cov[a][a], 0.0)) for a in range(k)]
    return beta, se, r2

def mat(a, b):
    return [[sum(a[i][t] * b[t][j] for t in range(len(b))) for j in range(len(b[0]))] for i in range(len(a))]

def invert(m):
    n = len(m)
    a = [row[:] + [1.0 if i == j else 0.0 for j in range(n)] for i, row in enumerate(m)]
    for c in range(n):
        p = max(range(c, n), key=lambda r: abs(a[r][c]))
        a[c], a[p] = a[p], a[c]
        piv = a[c][c]
        if abs(piv) < 1e-300:
            raise ValueError('singular')
        a[c] = [v / piv for v in a[c]]
        for r in range(n):
            if r != c:
                f = a[r][c]
                a[r] = [vr - f * vc for vr, vc in zip(a[r], a[c])]
    return [row[n:] for row in a]

def cks(interval_ms, lags):
    rows = {v: [] for v in VENUES}
    for s in SESSIONS:
        series = {v: [] for v in VENUES}
        for line in lre('ofi-series', os.path.join(REC, s), str(interval_ms))[1:]:
            k, v, ofi, ti, m0, m1, depth, upd = line.split(',')
            v = int(v)
            if m0 and m1 and depth:
                series[v].append(dict(k=int(k), ofi=int(ofi), ti=int(ti), dp=(int(m1) - int(m0)) / 2, depth=float(depth)))
            else:
                series[v].append(None)
        for v in VENUES:
            rows[v].append(series[v])
    out = {}
    for v, name in VENUES.items():
        flat = [r for sess in rows[v] for r in sess if r]
        dp = [r['dp'] for r in flat]
        ofi = [r['ofi'] / UNITS_PER_BTC for r in flat]
        ti = [r['ti'] / UNITS_PER_BTC for r in flat]
        b_ofi, se_ofi, r2_ofi = ols([ofi], dp, lags)
        _, _, r2_ti = ols([ti], dp, lags)
        _, _, r2_both = ols([ofi, ti], dp, lags)
        # Depth relation per 5-minute block.
        block = max(1, 300_000 // interval_ms)
        logb, logd = [], []
        for sess in rows[v]:
            for start in range(0, len(sess), block):
                chunk = [r for r in sess[start:start + block] if r]
                if len(chunk) < max(20, block // 2):
                    continue
                try:
                    b, _, _ = ols([[r['ofi'] / UNITS_PER_BTC for r in chunk]], [r['dp'] for r in chunk], 0)
                except ValueError:
                    continue
                d = sum(r['depth'] for r in chunk) / len(chunk) / UNITS_PER_BTC
                if b[1] > 0 and d > 0:
                    logb.append(math.log(b[1])); logd.append(math.log(d))
        depth_fit = None
        if len(logb) >= 5:
            bd, sd, r2d = ols([logd], logb, 0)
            depth_fit = dict(blocks=len(logb), slope=bd[1], intercept=bd[0], slope_se=sd[1], r2=r2d,
                             points=[[round(a, 4), round(b, 4)] for a, b in zip(logd, logb)])
        # Binned response: mean dP within 20 equal-count OFI bins (for the figure).
        idx = sorted(range(len(ofi)), key=lambda i: ofi[i])
        bins = []
        for q in range(20):
            part = idx[q * len(idx) // 20:(q + 1) * len(idx) // 20]
            if part:
                bins.append([sum(ofi[i] for i in part) / len(part), sum(dp[i] for i in part) / len(part)])
        # Predictive: next interval's price change on this interval's OFI (within sessions).
        x, y = [], []
        for sess in rows[v]:
            for a, b in zip(sess, sess[1:]):
                if a and b:
                    x.append(a['ofi'] / UNITS_PER_BTC); y.append(b['dp'])
        bp, sep, r2p = ols([x], y, lags)
        order = sorted(range(len(x)), key=lambda i: abs(x[i]))
        top = order[int(len(order) * 0.9):]
        follow = [y[i] * (1 if x[i] > 0 else -1) for i in top if x[i] != 0]
        out[name] = dict(n=len(dp), beta_ticks_per_btc=b_ofi[1], beta_t=b_ofi[1] / se_ofi[1] if se_ofi[1] else None,
                         r2_ofi=r2_ofi, r2_ti=r2_ti, r2_both=r2_both, depth=depth_fit, bins=bins, alpha=b_ofi[0],
                         predictive=dict(n=len(x), beta=bp[1], t=bp[1] / sep[1] if sep[1] else None, r2=r2p,
                                         top_decile_follow_ticks=sum(follow) / len(follow) if follow else None,
                                         taker_round_trip_ticks=TAKER_ROUND_TRIP_TICKS))
        print(f'CKS {interval_ms} ms {name}: n={len(dp)} beta={b_ofi[1]:.2f} ticks/BTC t={out[name]["beta_t"]:.1f} '
              f'R2 OFI={r2_ofi:.3f} TI={r2_ti:.3f} both={r2_both:.3f} depth={depth_fit and round(depth_fit["slope"], 2)} '
              f'pred R2={r2p:.4f} t={out[name]["predictive"]["t"]:.2f} follow={out[name]["predictive"]["top_decile_follow_ticks"]:.1f}t')
    return out

def quantile(xs, q):
    s = sorted(xs)
    return s[min(len(s) - 1, int(q * (len(s) - 1)))] if s else None

def farmer():
    out = {}
    orders = {v: [] for v in VENUES}
    for s in SESSIONS:
        for line in lre('impact-events', os.path.join(REC, s))[1:]:
            t, v, side, prints, qty, p0, p1, best, bq, second, sq, d5, m0, m1, ba = line.split(',')
            v, d = int(v), (1 if side == 'B' else -1)
            best, bq, second, ba = int(best), int(bq), int(second), int(ba)
            if best == 0 or bq == 0:
                continue
            orders[v].append(dict(dp=(int(m1) - int(m0)) / 2 * d, qty=int(qty), ratio=int(qty) / bq,
                                  gap=abs(second - best) if second else None,
                                  cleared=(ba > best) if d > 0 else (ba < best)))
    for v, name in VENUES.items():
        o = orders[v]
        moved = [x for x in o if x['dp'] > 0]
        cleared = [x for x in o if x['cleared'] and x['gap'] and x['dp'] > 0]
        lx_gap = [math.log(x['gap']) for x in cleared]
        lx_size = [math.log(x['ratio']) for x in cleared]
        ly = [math.log(x['dp']) for x in cleared]
        r2_gap = ols([lx_gap], ly, 0)[2] if len(cleared) > 10 else None
        r2_size = ols([lx_size], ly, 0)[2] if len(cleared) > 10 else None
        joint = ols([lx_gap, lx_size], ly, 0) if len(cleared) > 10 else None
        thr = quantile([abs(x['dp']) for x in o], 0.99)
        large = [x for x in o if abs(x['dp']) >= thr and x['dp'] > 0] if thr else []
        med = lambda xs: quantile(xs, 0.5)
        out[name] = dict(
            orders=len(o), moved=len(moved), cleared=len(cleared),
            share_moves_from_cleared=len([x for x in moved if x['cleared']]) / len(moved) if moved else None,
            r2_log_move_on_log_gap=r2_gap, r2_log_move_on_log_size=r2_size,
            joint=None if not joint else dict(gap_coef=joint[0][1], gap_se=joint[1][1], size_coef=joint[0][2], size_se=joint[1][2], r2=joint[2]),
            large=dict(threshold_ticks=thr, n=len(large),
                       median_size_ratio=med([x['ratio'] for x in large]),
                       share_smaller_than_best=len([x for x in large if x['ratio'] < 1]) / len(large) if large else None,
                       median_gap=med([x['gap'] for x in large if x['gap']]),
                       median_qty_btc=(med([x['qty'] for x in large]) or 0) / UNITS_PER_BTC),
            all=dict(median_size_ratio=med([x['ratio'] for x in o]), median_gap=med([x['gap'] for x in o if x['gap']]),
                     median_qty_btc=(med([x['qty'] for x in o]) or 0) / UNITS_PER_BTC),
            quantiles=dict(move_cleared={q: quantile([x['dp'] for x in cleared], q) for q in (0.5, 0.9, 0.99)},
                           half_gap={q: (quantile([x['gap'] for x in cleared], q) or 0) / 2 for q in (0.5, 0.9, 0.99)}))
        L = out[name]['large']
        print(f'Farmer {name}: orders={len(o)} moved={len(moved)} cleared={len(cleared)} R2 gap={r2_gap and round(r2_gap, 3)} '
              f'size={r2_size and round(r2_size, 3)} | top1% moves n={L["n"]} median size/best={L["median_size_ratio"]} '
              f'smaller-than-best={L["share_smaller_than_best"] and round(L["share_smaller_than_best"], 2)} median gap={L["median_gap"]} vs all {out[name]["all"]["median_gap"]}')
    return out

def farmer_intervals():
    """Interval version of the Farmer et al. claim: for the largest 1% of one-second price
    changes, could the traded volume alone have consumed the touch? Compares |dP| explained by
    traded volume (|TI|) with |dP| explained by net order-book flow (|OFI|, which also counts
    cancellations and new quotes at the touch)."""
    out = {}
    rows = {v: [] for v in VENUES}
    for s in SESSIONS:
        for line in lre('ofi-series', os.path.join(REC, s), '1000')[1:]:
            k, v, ofi, ti, m0, m1, depth, upd = line.split(',')
            if m0 and m1 and depth:
                rows[int(v)].append(dict(dp=abs(int(m1) - int(m0)) / 2, ti=abs(int(ti)), ofi=abs(int(ofi)), depth=float(depth)))
    for v, name in VENUES.items():
        r = [x for x in rows[v] if x['depth'] > 0]
        thr = quantile([x['dp'] for x in r], 0.99)
        large = [x for x in r if x['dp'] >= thr and x['dp'] > 0]
        r2_ti = ols([[x['ti'] / UNITS_PER_BTC for x in r]], [x['dp'] for x in r], 5)[2]
        r2_ofi = ols([[x['ofi'] / UNITS_PER_BTC for x in r]], [x['dp'] for x in r], 5)[2]
        out[name] = dict(intervals=len(r), large_threshold_ticks=thr, large=len(large),
                         share_large_volume_below_touch=sum(1 for x in large if x['ti'] < x['depth']) / len(large) if large else None,
                         median_large_volume_over_touch=quantile([x['ti'] / x['depth'] for x in large], 0.5),
                         median_all_volume_over_touch=quantile([x['ti'] / x['depth'] for x in r], 0.5),
                         r2_abs_move_on_abs_ti=r2_ti, r2_abs_move_on_abs_ofi=r2_ofi)
        o = out[name]
        print(f'Farmer intervals {name}: n={len(r)} top1% |dP|>={thr}t n={len(large)} volume<touch in {100 * o["share_large_volume_below_touch"]:.0f}% '
              f'median vol/touch large={o["median_large_volume_over_touch"]:.2f} all={o["median_all_volume_over_touch"]:.2f} '
              f'R2 |dP|~|TI|={r2_ti:.3f} |dP|~|OFI|={r2_ofi:.3f}')
    return out

def divergence():
    out = {}
    pairs = [('binance', 'bybit'), ('binance', 'okx'), ('bybit', 'okx')]
    gaps = {p: [] for p in pairs}
    demeaned = {p: [] for p in pairs}
    for s in SESSIONS:
        rows = lre('series', os.path.join(REC, s), '200')[1:]
        cols = {'binance': [], 'bybit': [], 'okx': []}
        for line in rows:
            f = line.split(',')
            for name, val in zip(('binance', 'bybit', 'okx'), f[3:6]):
                cols[name].append(float(val) if val else None)
        for a, b in pairs:
            g = [x - y for x, y in zip(cols[a], cols[b]) if x is not None and y is not None]
            if not g:
                continue
            m = sum(g) / len(g)
            gaps[(a, b)] += g
            demeaned[(a, b)] += [x - m for x in g]
    for (a, b), g in gaps.items():
        d = demeaned[(a, b)]
        absg = [abs(x) for x in g]
        edges = list(range(0, 1001, 25))
        hist = [sum(1 for x in absg if lo <= x < lo + 25) for lo in edges[:-1]] + [sum(1 for x in absg if x >= 1000)]
        out[f'{a}-{b}'] = dict(hist_edges_ticks=edges, hist=hist, p50_abs=quantile(absg, 0.5),samples=len(g), mean_ticks=sum(g) / len(g), sd_ticks=math.sqrt(sum((x - sum(g) / len(g)) ** 2 for x in g) / len(g)),
                               p99_abs=quantile(absg, 0.99), max_abs=max(absg),
                               share_over_maker=sum(1 for x in absg if x >= MAKER_TWO_VENUE_TICKS) / len(g),
                               share_over_taker=sum(1 for x in absg if x >= TAKER_TWO_VENUE_TICKS) / len(g),
                               demeaned_max_abs=max(abs(x) for x in d))
        o = out[f'{a}-{b}']
        print(f'Gap {a}-{b}: n={o["samples"]} mean={o["mean_ticks"]:.1f}t sd={o["sd_ticks"]:.1f}t p99|gap|={o["p99_abs"]:.1f}t '
              f'max={o["max_abs"]:.1f}t over maker hurdle {MAKER_TWO_VENUE_TICKS}t: {100 * o["share_over_maker"]:.3f}%')
    return dict(pairs=out, hurdles=dict(maker_two_venue_ticks=MAKER_TWO_VENUE_TICKS, taker_two_venue_ticks=TAKER_TWO_VENUE_TICKS))

result = dict(sessions=SESSIONS, cks={'1s': cks(1000, 5), '10s': cks(10000, 2)}, farmer=farmer(),
              farmer_intervals=farmer_intervals(), divergence=divergence())
path = os.path.join(REPO, 'docs/evidence/replications.json')
json.dump(result, open(path, 'w'), indent=1)
print('wrote', path)
