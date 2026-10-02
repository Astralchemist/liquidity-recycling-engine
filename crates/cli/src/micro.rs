//! Microstructure measurements for the paper replications (docs/study), using the engine's own
//! books and the production best-quote OFI (`orderflow::best_quote_ofi`, Cont–Kukanov–Stoikov
//! §2.1) over recorded sessions. Output is CSV on stdout in grid units: ticks of the common
//! price grid and quantity units of the common quantity grid.
use super::live::{LiveEngine, replay_with};
use book::{BookState, Level};
use common::{Side, VenueId};
use market_events::{MarketEvent, MarketEventType as K};
use orderflow::{BestQuotes, best_quote_ofi};
const VENUES: usize = 3;
/// Up to `n` live levels on `side` of venue index `v`, normalized to the grid.
fn levels(e: &LiveEngine, v: usize, side: Side, n: usize) -> Option<Vec<Level>> {
    let book = e
        .research()
        .research()
        .market()
        .venue_book(VenueId(v as u16 + 1))
        .ok()?;
    if book.state() != BookState::Live {
        return None;
    }
    let norm = e.normalizers()[v];
    book.levels(side)
        .ok()?
        .iter()
        .take(n)
        .map(|l| {
            Some(Level {
                price: norm.price(l.price).ok()?,
                qty: norm.quantity(l.qty).ok()?,
            })
        })
        .collect()
}
fn best(e: &LiveEngine, v: usize) -> Option<BestQuotes> {
    Some(BestQuotes {
        bid: *levels(e, v, Side::Buy, 1)?.first()?,
        ask: *levels(e, v, Side::Sell, 1)?.first()?,
    })
}
fn mid_x2(q: BestQuotes) -> i128 {
    i128::from(q.bid.price.0) + i128::from(q.ask.price.0)
}
#[derive(Default, Clone, Copy)]
struct Bucket {
    ofi: i128,
    ti: i128,
    depth_sum: i128,
    depth_n: i64,
    updates: u64,
    start: Option<i128>,
}
/// Per venue and interval: the production best-quote OFI, trade imbalance (aggressive buy minus
/// sell quantity), the venue midpoint at the interval's start and end, and mean touch depth.
pub fn ofi_series(directory: &str, interval_ms: &str) -> Result<(), String> {
    let step = interval_ms
        .parse::<u64>()
        .ok()
        .filter(|&s| s >= 100)
        .ok_or("INTERVAL_MS must be an integer of at least 100")?
        * 1_000_000;
    println!("k,venue,ofi,ti,mid_start_x2,mid_end_x2,mean_depth,updates");
    let mut buckets = [Bucket::default(); VENUES];
    let (mut first, mut k) = (None::<u64>, 0_u64);
    let mut before: [Option<BestQuotes>; VENUES] = [None; VENUES];
    let flush = |k: u64, b: &mut [Bucket; VENUES], last: &[Option<BestQuotes>; VENUES]| {
        for (v, x) in b.iter_mut().enumerate() {
            let end = last[v].map(mid_x2);
            println!(
                "{k},{},{},{},{},{},{},{}",
                v + 1,
                x.ofi,
                x.ti,
                x.start.map_or(String::new(), |m| m.to_string()),
                end.map_or(String::new(), |m| m.to_string()),
                if x.depth_n > 0 {
                    format!("{:.1}", x.depth_sum as f64 / x.depth_n as f64)
                } else {
                    String::new()
                },
                x.updates
            );
            *x = Bucket {
                start: end,
                ..Bucket::default()
            };
        }
    };
    let mut state: [Option<BestQuotes>; VENUES] = [None; VENUES];
    replay_with(directory, |e, members: &[MarketEvent], applied| {
        let head = members[0];
        let v = head.venue.0 as usize - 1;
        let t = head.receive_ts.0;
        let start = *first.get_or_insert(t);
        if !applied {
            // Close every interval that ended before this frame.
            while (t - start) / step > k {
                flush(k, &mut buckets, &state);
                k += 1;
            }
            before[v] = best(e, v);
            return Ok(());
        }
        let after = best(e, v);
        if head.event_type == K::Trade {
            for m in members {
                let q = i128::from(
                    e.normalizers()[v]
                        .quantity(m.qty_units)
                        .map_err(|x| format!("{x:?}"))?
                        .0,
                );
                buckets[v].ti += if m.side == Side::Buy { q } else { -q };
            }
        } else if let (Some(b0), Some(b1)) = (before[v], after) {
            buckets[v].ofi += best_quote_ofi(b0, b1).map_err(|x| format!("{x:?}"))?;
            buckets[v].updates += 1;
        }
        if let Some(q) = after {
            buckets[v].depth_sum += i128::from(q.bid.qty.0 + q.ask.qty.0) / 2;
            buckets[v].depth_n += 1;
            if buckets[v].start.is_none() {
                buckets[v].start = Some(mid_x2(q));
            }
        }
        state[v] = after;
        Ok(())
    })?;
    flush(k, &mut buckets, &state);
    Ok(())
}
#[derive(Clone)]
struct Order {
    t: u64,
    side: Side,
    prints: u32,
    qty: i64,
    first: i64,
    last: i64,
    hit: Vec<Level>,
    mid_before: i128,
}
/// One row per market order (the prints of one venue message with one aggressor side), with the
/// book it hit just before and the venue midpoint after the venue's next depth update.
pub fn impact_events(directory: &str) -> Result<(), String> {
    println!(
        "t_ms,venue,side,prints,qty,first_price,last_price,best,best_qty,second,second_qty,depth5,mid_before_x2,mid_after_x2,best_after"
    );
    let mut pending: [Vec<Order>; VENUES] = [Vec::new(), Vec::new(), Vec::new()];
    let mut first = None::<u64>;
    replay_with(directory, |e, members: &[MarketEvent], applied| {
        let head = members[0];
        let v = head.venue.0 as usize - 1;
        let start = *first.get_or_insert(head.receive_ts.0);
        if head.event_type == K::Trade {
            if applied {
                return Ok(());
            }
            let (Some(q), Some(bids), Some(asks)) = (
                best(e, v),
                levels(e, v, Side::Buy, 5),
                levels(e, v, Side::Sell, 5),
            ) else {
                return Ok(());
            };
            for m in members {
                let side = m.side;
                let qty = e.normalizers()[v]
                    .quantity(m.qty_units)
                    .map_err(|x| format!("{x:?}"))?
                    .0;
                let price = e.normalizers()[v]
                    .price(m.price_ticks)
                    .map_err(|x| format!("{x:?}"))?
                    .0;
                match pending[v].last_mut() {
                    Some(o) if o.t == m.receive_ts.0 && o.side == side => {
                        o.prints += 1;
                        o.qty += qty;
                        o.last = price;
                    }
                    _ => pending[v].push(Order {
                        t: m.receive_ts.0,
                        side,
                        prints: 1,
                        qty,
                        first: price,
                        last: price,
                        // A buy aggressor hits the asks.
                        hit: if side == Side::Buy {
                            asks.clone()
                        } else {
                            bids.clone()
                        },
                        mid_before: mid_x2(q),
                    }),
                }
            }
            return Ok(());
        }
        if !applied || pending[v].is_empty() {
            return Ok(());
        }
        let Some(q) = best(e, v) else {
            pending[v].clear();
            return Ok(());
        };
        for o in pending[v].drain(..) {
            let (b0, b1) = (o.hit.first(), o.hit.get(1));
            let best_after = if o.side == Side::Buy {
                q.ask.price
            } else {
                q.bid.price
            };
            println!(
                "{:.3},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                (o.t - start) as f64 / 1e6,
                v + 1,
                if o.side == Side::Buy { "B" } else { "S" },
                o.prints,
                o.qty,
                o.first,
                o.last,
                b0.map_or(0, |l| l.price.0),
                b0.map_or(0, |l| l.qty.0),
                b1.map_or(0, |l| l.price.0),
                b1.map_or(0, |l| l.qty.0),
                o.hit.iter().map(|l| l.qty.0).sum::<i64>(),
                o.mid_before,
                mid_x2(q),
                best_after.0
            );
        }
        Ok(())
    })
}
