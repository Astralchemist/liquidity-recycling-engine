#!/usr/bin/env python3
"""Momentum-continuation study: after a "mega pump" candle, does the next candle move in the
same direction, how often, and for how long? Reads trade prints straight from recordings
(format v2, docs/recording-format.md); standard library only.

Usage: scripts/momentum.py RECORDINGS_DIR [OUT_JSON]

Definitions (all stated, none fitted):
- Candles: OHLC of one venue's trade prints by local receive time, at 1 s, 5 s, 15 s and 60 s.
  A candle without prints carries the previous close (return 0).
- Return r_i = close_i - close_{i-1}, in ticks of 0.1 USDT.
- Mega pump: |r_i| >= k * sigma_i, where sigma_i is the standard deviation of the PREVIOUS 120
  candle returns (at least 30), i.e. only past data, as a live detector would. k = 3 and 5.
- Continuation: the next candle's return has the pump's sign. Base rate: the same probability
  over all consecutive nonzero candle pairs. Zero-return next candles are counted separately.
- Run: consecutive candles after the pump moving in the pump's direction; a zero or opposite
  candle ends it. Duration = run length x candle length.
- Excursion: the furthest trade price in the pump's direction during the run, from the pump close.
- Taker test: enter at the pump close, exit at the close m candles later (m = 1, 2, 5), paying
  the venue taker fee on both sides (Binance and OKX 500 ppm, Bybit 550 ppm). This is an upper
  bound on what momentum entry could capture: real fills cross the spread after a latency.
"""
import json, math, os, struct, sys

VENUES = {1: 'binance', 2: 'bybit', 3: 'okx'}
TAKER_PPM = {1: 500, 2: 550, 3: 500}
SESSIONS = ['session-A', 'session-B', 'session-C', 'session-D', 'session-E']
CANDLES_S = [1, 5, 15, 60]
KS = [3, 5]
WINDOW, MIN_WINDOW = 120, 30

def trades(path):
    """(receive_ns, price_ticks) of every trade print, in file order."""
    b = open(path, 'rb').read()
    out, i = [], 48
    while i + 16 <= len(b):
        if b[i:i + 4] != b'LRFR':
            raise SystemExit(f'{path}: bad frame at {i}')
        n = struct.unpack_from('<I', b, i + 8)[0]
        i += 16
        for k in range(n):
            o = i + 60 * k
            if b[o + 38] == 3:
                out.append((struct.unpack_from('<Q', b, o + 30)[0], struct.unpack_from('<q', b, o + 40)[0]))
        i += 60 * n
    return out

def candles_exact(prints, seconds):
    """High, low and close per candle; a candle without prints repeats the previous close."""
    step = seconds * 1_000_000_000
    start = prints[0][0]
    n = (prints[-1][0] - start) // step + 1
    hi, lo, cl = [None] * n, [None] * n, [None] * n
    for t, p in prints:
        k = (t - start) // step
        hi[k] = p if hi[k] is None else max(hi[k], p)
        lo[k] = p if lo[k] is None else min(lo[k], p)
        cl[k] = p
    prev = prints[0][1]
    for k in range(n):
        if cl[k] is None:
            hi[k] = lo[k] = cl[k] = prev
        prev = cl[k]
    return hi, lo, cl

def study(hi, lo, cl, seconds, k, venue):
    r = [0] + [cl[i] - cl[i - 1] for i in range(1, len(cl))]
    events = []
    for i in range(1, len(r) - 1):
        past = r[max(1, i - WINDOW):i]
        if len(past) < MIN_WINDOW:
            continue
        mean = sum(past) / len(past)
        sd = math.sqrt(sum((x - mean) ** 2 for x in past) / (len(past) - 1))
        if sd == 0 or abs(r[i]) < k * sd:
            continue
        d = 1 if r[i] > 0 else -1
        run, j = 0, i + 1
        while j < len(r) and r[j] * d > 0:
            run += 1
            j += 1
        # Furthest price in the pump direction during the run (0 if no run).
        exc = 0
        for m in range(i + 1, i + 1 + run):
            exc = max(exc, (hi[m] - cl[i]) if d > 0 else (cl[i] - lo[m]))
        fwd = {}
        for m in (1, 2, 5):
            if i + m < len(cl):
                gross = (cl[i + m] - cl[i]) * d
                fee = (cl[i] + cl[i + m]) * TAKER_PPM[venue] / 1e6  # both sides, in ticks per unit
                fwd[m] = (gross, gross - fee)
        events.append(dict(i=i, dir=d, size=abs(r[i]), sigma=sd, next=r[i + 1] * d, run=run, exc=exc, fwd=fwd))
    pairs = [(r[i], r[i + 1]) for i in range(1, len(r) - 1) if r[i] != 0 and r[i + 1] != 0]
    base_same = sum(1 for a, b in pairs if (a > 0) == (b > 0))
    return events, len(pairs), base_same

def summarise(events, pairs, base_same, hours, seconds):
    n = len(events)
    cont = sum(1 for e in events if e['next'] > 0)
    flat = sum(1 for e in events if e['next'] == 0)
    rev = n - cont - flat
    nz = cont + rev
    p = cont / nz if nz else None
    base = base_same / pairs if pairs else None
    z = None
    if nz and base not in (None, 0, 1):
        z = (p - base) / math.sqrt(base * (1 - base) / nz)
    runs = [e['run'] for e in events]
    cont_runs = [e['run'] for e in events if e['run'] > 0]
    fwd = {}
    for m in (1, 2, 5):
        xs = [e['fwd'][m] for e in events if m in e['fwd']]
        if xs:
            g = [a for a, _ in xs]; net = [b for _, b in xs]
            sd = math.sqrt(sum((x - sum(net) / len(net)) ** 2 for x in net) / max(len(net) - 1, 1))
            fwd[m] = dict(n=len(xs), gross=sum(g) / len(g), net=sum(net) / len(net), net_se=sd / math.sqrt(len(net)),
                          positive_net=sum(1 for x in net if x > 0))
    return dict(events=n, per_hour=n / hours if hours else None, continued=cont, reversed=rev, flat=flat,
                p_continue=p, base_rate=base, z=z, base_pairs=pairs,
                mean_run_candles=sum(runs) / n if n else None,
                mean_duration_s_when_continued=(sum(cont_runs) / len(cont_runs) * seconds) if cont_runs else None,
                median_run_when_continued=(sorted(cont_runs)[len(cont_runs) // 2] if cont_runs else None),
                mean_excursion_ticks_when_continued=(sum(e['exc'] for e in events if e['run'] > 0) / len(cont_runs)) if cont_runs else None,
                mean_pump_ticks=sum(e['size'] for e in events) / n if n else None,
                forward=fwd, runs=runs)

def main():
    rec = os.path.abspath(sys.argv[1])
    out_path = sys.argv[2] if len(sys.argv) > 2 else None
    data = dict(definitions=__doc__, results={})
    prints = {(s, v): trades(os.path.join(rec, s, f'venue-{v}.lre')) for s in SESSIONS for v in VENUES}
    hours = {s: (max(t for t, _ in prints[(s, 1)]) - min(t for t, _ in prints[(s, 1)])) / 3.6e12 for s in SESSIONS}
    print('trade prints per session/venue:', {f'{s[-1]}/{VENUES[v]}': len(p) for (s, v), p in prints.items()})
    for v, name in VENUES.items():
        for seconds in CANDLES_S:
            for k in KS:
                evs, pairs, same, total_h = [], 0, 0, 0.0
                for s in SESSIONS:
                    p = prints[(s, v)]
                    if len(p) < 2:
                        continue
                    hi, lo, cl = candles_exact(p, seconds)
                    e, n_pairs, n_same = study(hi, lo, cl, seconds, k, v)
                    for x in e:
                        x['session'] = s[-1]
                    evs += e; pairs += n_pairs; same += n_same; total_h += hours[s]
                res = summarise(evs, pairs, same, total_h, seconds)
                res['by_session'] = {s[-1]: sum(1 for x in evs if x['session'] == s[-1]) for s in SESSIONS}
                data['results'][f'{name}|{seconds}s|{k}sigma'] = res
                f1 = res['forward'].get(1, {})
                print(f"{name:7} {seconds:>2}s k={k}: pumps={res['events']:4d} ({res['per_hour'] or 0:5.1f}/h) "
                      f"continue={res['continued']:4d} reverse={res['reversed']:4d} flat={res['flat']:4d} "
                      f"P(cont)={(res['p_continue'] or 0):.3f} base={(res['base_rate'] or 0):.3f} z={(res['z'] or 0):+.2f} "
                      f"run={(res['mean_duration_s_when_continued'] or 0):6.1f}s exc={(res['mean_excursion_ticks_when_continued'] or 0):6.1f}t "
                      f"next1 gross={f1.get('gross', 0):+7.1f}t net={f1.get('net', 0):+8.1f}t")
    if out_path:
        json.dump(data, open(out_path, 'w'), separators=(',', ':'))
        print('wrote', out_path)

if __name__ == '__main__':
    main()
