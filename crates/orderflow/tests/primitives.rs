use book::Level;
use common::{Side, Timestamp};
use fixed_point::{PriceTicks, QtyUnits};
use orderflow::*;
fn best(b: i64, bq: i64, a: i64, aq: i64) -> BestQuotes {
    BestQuotes {
        bid: Level {
            price: PriceTicks(b),
            qty: QtyUnits(bq),
        },
        ask: Level {
            price: PriceTicks(a),
            qty: QtyUnits(aq),
        },
    }
}
fn cfg() -> FlowConfig {
    FlowConfig {
        event_window: 7,
        time_window_ns: 20,
        qi_levels: 2,
        removal_attribution: RemovalAttribution::Unknown,
    }
}
const METRICS: [Metric; 12] = [
    Metric::Ofi,
    Metric::DepthDelta,
    Metric::AddedQty,
    Metric::RemovedQty,
    Metric::BuyQty,
    Metric::SellQty,
    Metric::AddEvents,
    Metric::RemovalEvents,
    Metric::BuyEvents,
    Metric::SellEvents,
    Metric::CancelQty,
    Metric::CancelEvents,
];
#[test]
fn best_quote_ofi_all_price_relations_and_extreme_sizes() {
    for b in 99..=101 {
        for a in 109..=111 {
            let expected_bid = match b {
                99 => -20,
                100 => 30 - 20,
                _ => 30,
            };
            let expected_ask = match a {
                109 => -50,
                110 => 40 - 50,
                _ => 40,
            };
            assert_eq!(
                best_quote_ofi(best(100, 20, 110, 40), best(b, 30, a, 50)).unwrap(),
                expected_bid + expected_ask
            );
        }
    }
    assert_eq!(
        best_quote_ofi(
            best(100, i64::MAX, 110, i64::MAX),
            best(99, 1, 109, i64::MAX)
        )
        .unwrap(),
        -2 * i128::from(i64::MAX)
    );
    assert!(best_quote_ofi(best(100, 0, 110, 1), best(100, 1, 110, 1)).is_err());
}
#[test]
fn queue_imbalance_bounds_symmetry_and_undefined_empty() {
    assert_eq!(queue_imbalance(0, 0), Ok(None));
    assert_eq!(queue_imbalance(1, 0), Ok(Some(1_000_000)));
    assert_eq!(queue_imbalance(1, 3), Ok(Some(-500_000)));
    assert!(queue_imbalance(-1, 1).is_err());
    assert!(queue_imbalance(i128::MAX, 1).is_err());
    let mut x = 17_u64;
    for _ in 0..10000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        let (a, b) = (i128::from(x), i128::from(x.rotate_left(19)));
        let q = queue_imbalance(a, b).unwrap().unwrap();
        assert!((-1_000_000..=1_000_000).contains(&q));
        assert_eq!(q, -queue_imbalance(b, a).unwrap().unwrap());
    }
}
#[test]
fn generated_windows_match_independent_brute_force_totals_and_buckets() {
    let mut e = FlowEngine::<32, 3>::new(cfg(), Timestamp(0)).unwrap();
    let (mut rng, mut now) = (19_u64, 0_u64);
    let mut history = Vec::new();
    for _ in 0..10000 {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        now += 1 + rng % 3;
        let side = if rng % 2 == 0 { Side::Buy } else { Side::Sell };
        let c = if rng % 3 == 0 {
            Contribution::trade(side, (rng % 100 + 1) as i64).unwrap()
        } else {
            Contribution::depth(
                (rng % 15) as i128 - 7,
                side,
                (rng % 201) as i128 - 100,
                RemovalAttribution::Unknown,
            )
            .unwrap()
        };
        let bucket = if rng % 4 == 0 {
            None
        } else {
            Some((rng % 3) as usize)
        };
        e.record(Timestamp(now), c, bucket).unwrap();
        history.push((now, c.totals(), bucket));
        let snap = e.snapshot().unwrap();
        for metric in METRICS {
            let recent = &history[history.len().saturating_sub(7)..];
            let timed = history
                .iter()
                .rev()
                .take_while(|h| now - h.0 < 20)
                .collect::<Vec<_>>();
            assert_eq!(
                snap.event_totals.get(metric),
                recent.iter().map(|h| h.1.get(metric)).sum::<i128>()
            );
            assert_eq!(
                snap.time_totals.get(metric),
                timed.iter().map(|h| h.1.get(metric)).sum::<i128>()
            );
            for b in 0..3 {
                assert_eq!(
                    snap.event_buckets[b].get(metric),
                    recent
                        .iter()
                        .filter(|h| h.2 == Some(b))
                        .map(|h| h.1.get(metric))
                        .sum::<i128>()
                );
                assert_eq!(
                    snap.time_buckets[b].get(metric),
                    timed
                        .iter()
                        .filter(|h| h.2 == Some(b))
                        .map(|h| h.1.get(metric))
                        .sum::<i128>()
                );
            }
        }
    }
    e.advance_time(Timestamp(now + 20)).unwrap();
    assert_eq!(e.snapshot().unwrap().time_count, 0);
    assert_eq!(e.snapshot().unwrap().event_count, 7);
}
#[test]
fn same_timestamp_capacity_exposure_expiry_and_fault_latching() {
    let mut config = cfg();
    config.event_window = 2;
    let mut e = FlowEngine::<2, 1>::new(config, Timestamp(10)).unwrap();
    let c = Contribution::trade(Side::Buy, 5).unwrap();
    e.record(Timestamp(10), c, None).unwrap();
    assert_eq!(
        e.snapshot().unwrap().time_rate(Metric::BuyEvents).unwrap(),
        None
    );
    e.record(Timestamp(10), c, None).unwrap();
    let mut full = e.clone();
    assert_eq!(
        full.record(Timestamp(10), c, None),
        Err(FlowError::Capacity)
    );
    assert_eq!(full.snapshot(), Err(FlowError::Faulted));
    e.advance_time(Timestamp(29)).unwrap();
    assert_eq!(e.snapshot().unwrap().time_count, 2);
    e.record(Timestamp(30), c, None).unwrap();
    let s = e.snapshot().unwrap();
    assert_eq!(s.time_count, 1);
    assert_eq!(s.event_count, 2);
    assert_eq!(s.exposure_ns, 20);
    assert_eq!(
        s.time_rate(Metric::BuyEvents).unwrap(),
        Some(50_000_000_000_000)
    );
    assert_eq!(
        e.advance_time(Timestamp(29)),
        Err(FlowError::ClockRegression)
    );
    assert_eq!(e.record(Timestamp(31), c, None), Err(FlowError::Faulted));
}
#[test]
fn attribution_requires_explicit_assumption_and_matching_configuration() {
    let c = Contribution::depth(-10, Side::Buy, -10, RemovalAttribution::Unknown).unwrap();
    let mut e = FlowEngine::<8, 1>::new(cfg(), Timestamp(0)).unwrap();
    e.record(Timestamp(1), c, None).unwrap();
    let s = e.snapshot().unwrap();
    assert_eq!(s.time_totals.get(Metric::RemovedQty), 10);
    assert_eq!(s.cancellation_rate().unwrap(), None);
    assert_eq!(s.time_rate(Metric::CancelEvents).unwrap(), None);
    assert_eq!(s.bucket_time_rate(0, Metric::CancelQty).unwrap(), None);
    assert_eq!(
        s.bucket_time_rate(1, Metric::AddedQty),
        Err(FlowError::InvalidObservation)
    );
    let mut config = cfg();
    config.removal_attribution = RemovalAttribution::AssumeCancellation;
    let mut e = FlowEngine::<8, 1>::new(config, Timestamp(0)).unwrap();
    let c = Contribution::depth(-10, Side::Buy, -10, config.removal_attribution).unwrap();
    e.record(Timestamp(1), c, Some(0)).unwrap();
    assert_eq!(
        e.snapshot().unwrap().cancellation_rate().unwrap(),
        Some(1_000_000_000_000_000)
    );
    assert!(
        e.record(
            Timestamp(2),
            Contribution::depth(0, Side::Buy, 1, RemovalAttribution::Unknown).unwrap(),
            None
        )
        .is_err()
    );
}
#[test]
fn poisson_fixed_point_matches_exp_and_is_monotone() {
    let mut prior = 0;
    for i in 0..=20000 {
        let rate = i * 1000;
        let actual = poisson_event_probability_ppm(rate, 1_000_000_000);
        let expected = (1.0 - (-(rate as f64) / 1_000_000.0).exp()) * 1_000_000.0;
        assert!(
            (f64::from(actual) - expected).abs() <= 2.0,
            "rate={rate} actual={actual} reference={expected}"
        );
        assert!(actual >= prior);
        prior = actual;
    }
    assert_eq!(poisson_event_probability_ppm(u64::MAX, u64::MAX), 999999);
    assert_eq!(poisson_event_probability_ppm(0, u64::MAX), 0);
    assert_eq!(poisson_event_probability_ppm(u64::MAX, 0), 0);
}
#[test]
fn event_bucket_boundary_is_half_open_and_side_aware() {
    let edges = [10_000, 20_000, 50_000];
    assert_eq!(distance_bucket(&edges, 200, 99, Side::Buy), Some(1));
    assert_eq!(distance_bucket(&edges, 200, 101, Side::Sell), Some(1));
    assert_eq!(distance_bucket(&edges, 200, 105, Side::Sell), None);
    assert_eq!(distance_bucket(&edges, 200, 101, Side::Buy), None);
    assert_eq!(distance_bucket(&edges, 201, 100, Side::Buy), Some(0));
}
#[test]
fn invalid_config_and_contribution_bounds_are_rejected() {
    let mut c = cfg();
    c.event_window = 0;
    assert!(FlowEngine::<8, 1>::new(c, Timestamp(0)).is_err());
    assert!(Contribution::trade(Side::Buy, 0).is_err());
    assert!(Contribution::depth(i128::MAX, Side::Buy, 0, RemovalAttribution::Unknown).is_err());
    assert!(Contribution::depth(0, Side::Buy, i128::MIN, RemovalAttribution::Unknown).is_err());
}
