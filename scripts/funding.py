#!/usr/bin/env python3
"""Cross-venue funding carry (alternative pattern 4). Standard library only.

Usage: scripts/funding.py EVIDENCE_DIR [--offline]

Fetches the public BTCUSDT perpetual funding history of Binance USD-M, Bybit linear and OKX
swap (no keys), stores the raw responses under EVIDENCE_DIR/funding/ (never committed), and
writes docs/evidence/funding.json.

Strategy, fixed before looking at the data:
- Pair (i, j). At each 8-hour settlement t, the position for the NEXT period is chosen from the
  rates just settled (no look-ahead): short the venue with the higher rate and long the other
  when |r_i - r_j| exceeds the hurdle, otherwise stay flat (or keep the position while the
  spread keeps its sign, to avoid churn).
- The position earns the next settlement's spread, |r_i - r_j| signed by the position, on one
  leg's notional (the hedge leg pays/receives the other rate).
- Opening or closing costs 2 legs x fee; flipping costs 4. Maker (0.02%) and taker (0.05%) fee
  cases are both reported.
- Price risk between venues (basis) is ignored: both legs are BTCUSDT perpetuals and their
  prices differ by tens of USDT at most (see the cross-venue gap test); funding is not.
Results are returns on ONE leg's notional; a hedged position needs margin on both legs.
"""
import json, os, sys, time, urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EVIDENCE = os.path.abspath(sys.argv[1])
OFFLINE = '--offline' in sys.argv
RAW = os.path.join(EVIDENCE, 'funding')
EIGHT_H_MS = 8 * 3600 * 1000

def get(url):
    req = urllib.request.Request(url, headers={'User-Agent': 'lre-research/0.1'})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode())

def binance(days):
    # Binance returns the EARLIEST `limit` rows of [startTime, endTime]: page forward.
    out, end = {}, int(time.time() * 1000)
    start = end - days * 86_400_000
    while start < end:
        rows = get(f'https://fapi.binance.com/fapi/v1/fundingRate?symbol=BTCUSDT&limit=1000&startTime={start}&endTime={end}')
        if not rows:
            break
        for r in rows:
            out[int(r['fundingTime'])] = float(r['fundingRate'])
        if len(rows) < 1000:
            break
        start = max(int(r['fundingTime']) for r in rows) + 1
    return out

def bybit(days):
    out, end = {}, int(time.time() * 1000)
    start = end - days * 86_400_000
    while end > start:
        rows = get(f'https://api.bybit.com/v5/market/funding/history?category=linear&symbol=BTCUSDT&limit=200&endTime={end}')['result']['list']
        if not rows:
            break
        for r in rows:
            out[int(r['fundingRateTimestamp'])] = float(r['fundingRate'])
        end = min(int(r['fundingRateTimestamp']) for r in rows) - 1
        if len(rows) < 200:
            break
    return {t: v for t, v in out.items() if t >= start}

def okx():
    out, after = {}, ''
    for _ in range(30):
        rows = get('https://www.okx.com/api/v5/public/funding-rate-history?instId=BTC-USDT-SWAP&limit=100' + (f'&after={after}' if after else ''))['data']
        if not rows:
            break
        for r in rows:
            out[int(r['fundingTime'])] = float(r['realizedRate'] or r['fundingRate'])
        after = str(min(int(r['fundingTime']) for r in rows))
        if len(rows) < 100:
            break
    return out

def fetch():
    os.makedirs(RAW, exist_ok=True)
    data = {}
    for name, fn in (('binance', lambda: binance(365)), ('bybit', lambda: bybit(365)), ('okx', okx)):
        path = os.path.join(RAW, f'{name}.json')
        if OFFLINE:
            data[name] = {int(k): v for k, v in json.load(open(path)).items()}
            continue
        rates = fn()
        json.dump({str(k): v for k, v in sorted(rates.items())}, open(path, 'w'))
        data[name] = rates
        print(f'{name}: {len(rates)} settlements from {time.strftime("%Y-%m-%d", time.gmtime(min(rates) / 1000))} '
              f'to {time.strftime("%Y-%m-%d", time.gmtime(max(rates) / 1000))}')
    return data

def align(a, b):
    """Settlements present on both venues, rounded to the 8-hour grid."""
    ra = {round(t / EIGHT_H_MS): v for t, v in a.items()}
    rb = {round(t / EIGHT_H_MS): v for t, v in b.items()}
    keys = sorted(set(ra) & set(rb))
    return keys, [ra[k] for k in keys], [rb[k] for k in keys]

def carry(ra, rb, fee, hurdle):
    """Returns per-period net returns (fraction of one leg's notional) and trade count."""
    pos, net, trades = 0, [], 0
    for t in range(len(ra) - 1):
        spread = ra[t] - rb[t]
        want = pos
        if pos == 0 and abs(spread) > hurdle:
            want = -1 if spread > 0 else 1   # short the higher-rate venue (a) when spread > 0
        elif pos != 0 and (spread * -pos) <= 0:
            want = 0                          # spread lost its sign: close
        cost = 0.0
        if want != pos:
            legs = 2 * abs(want - pos)       # open/close 2 legs, flip 4
            cost = legs * fee
            trades += 1
        pos = want
        # Earn next settlement: short a receives r_a, long b pays r_b.
        gain = -pos * (ra[t + 1] - rb[t + 1])
        net.append(gain - cost)
    if pos != 0:
        net[-1] -= 2 * fee                   # close at the end
    return net, trades

def summarise(net, periods_per_year=3 * 365):
    n = len(net)
    mean = sum(net) / n
    sd = (sum((x - mean) ** 2 for x in net) / (n - 1)) ** 0.5 if n > 1 else 0.0
    return dict(periods=n, total=sum(net), apr=mean * periods_per_year, sharpe=(mean / sd * periods_per_year ** 0.5) if sd else None)

data = fetch()
pairs = [('binance', 'bybit'), ('binance', 'okx'), ('bybit', 'okx')]
results = {}
for a, b in pairs:
    keys, ra, rb = align(data[a], data[b])
    spreads = [x - y for x, y in zip(ra, rb)]
    m = sum(spreads) / len(spreads)
    ac1 = (sum((spreads[i] - m) * (spreads[i + 1] - m) for i in range(len(spreads) - 1)) /
           sum((x - m) ** 2 for x in spreads)) if len(spreads) > 2 else None
    res = dict(settlements=len(keys), first=time.strftime('%Y-%m-%d', time.gmtime(keys[0] * EIGHT_H_MS / 1000)),
               last=time.strftime('%Y-%m-%d', time.gmtime(keys[-1] * EIGHT_H_MS / 1000)),
               mean_spread_bps=1e4 * m, mean_abs_spread_bps=1e4 * sum(abs(x) for x in spreads) / len(spreads),
               spread_autocorr_1=ac1,
               # Upper bound with hindsight: always on the side of the full-sample mean spread.
               gross_static_apr_hindsight=abs(m) * 3 * 365, strategies={})
    for fee_name, fee in (('maker', 0.0002), ('taker', 0.0005)):
        for hurdle_bps in (0.0, 1.0, 2.0, 5.0):
            net, trades = carry(ra, rb, fee, hurdle_bps / 1e4)
            cum, acc = [], 0.0
            for x in net:
                acc += x
                cum.append(round(1e4 * acc, 3))  # basis points of one leg's notional
            res['strategies'][f'{fee_name}|{hurdle_bps:g}bps'] = dict(trades=trades, cumulative_bps=cum[::3], **summarise(net))
    # Hindsight static carry, gross: always on the side of the full-sample mean spread.
    sign = 1 if m > 0 else -1
    acc, cum = 0.0, []
    for x in spreads[1:]:
        acc += sign * x
        cum.append(round(1e4 * acc, 3))
    res['hindsight_cumulative_bps'] = cum[::3]
    res['spread_bps_daily'] = [round(1e4 * sum(spreads[i:i + 3]) / len(spreads[i:i + 3]), 3) for i in range(0, len(spreads), 3)]
    results[f'{a}-{b}'] = res
    best = max(res['strategies'].items(), key=lambda kv: kv[1]['apr'])
    print(f'{a}-{b}: {res["settlements"]} settlements {res["first"]}..{res["last"]} mean spread {res["mean_spread_bps"]:+.3f} bp '
          f'|spread| {res["mean_abs_spread_bps"]:.3f} bp ac1={ac1:.2f} hindsight static gross APR {100 * res["gross_static_apr_hindsight"]:.2f}% '
          f'| best rule {best[0]}: APR {100 * best[1]["apr"]:+.2f}% trades={best[1]["trades"]}')
    for k, v in res['strategies'].items():
        print(f'   {k:12} trades={v["trades"]:4d} APR={100 * v["apr"]:+7.2f}% total={100 * v["total"]:+6.2f}% sharpe={v["sharpe"] and round(v["sharpe"], 2)}')
out = os.path.join(REPO, 'docs/evidence/funding.json')
json.dump(dict(pairs=results, note='Returns are on one leg notional; basis risk ignored; raw rates stay in the evidence folder.'), open(out, 'w'), indent=1)
print('wrote', out)
