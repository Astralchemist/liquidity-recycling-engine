use common::{Side, Timestamp};
use fixed_point::PriceTicks;
use liquidity::{grid::PriceGrid, *};
fn cfg() -> LiquidityConfig<3> {
    LiquidityConfig {
        lower_price: PriceTicks(80),
        cell_width: 4,
        bucket_edges_ppm: [100_000, 200_000, 500_000],
        sample_interval_ns: 1,
        warmup_samples: 3,
        baseline_alpha_ppm: 10_000,
        minimum_baseline_units: 1,
        minimum_covered_venues: 1,
        assume_contiguous_l2_coverage: true,
        pool_depth_ratio_ppm: 2_000_000,
        pool_persistence_ns: 3,
        pool_minimum_score_ppm: 1_100_000,
        pool_weights: [6, 2, 2],
        price_reference: PriceReference::Midpoint,
    }
}
#[test]
fn generated_grid_updates_match_brute_force_ranges() {
    let mut g = PriceGrid::<3, 64>::new(PriceTicks(80), 4, [1_000_000, 750_000, 500_000]).unwrap();
    let mut reference = [[[0_i64; 64]; 2]; 3];
    let mut rng = 19_u64;
    for time in 1..=20_000 {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let v = (rng >> 32) as usize % 3;
        let s = (rng >> 40) as usize % 2;
        let p = (rng >> 48) as usize % 64;
        let q = if rng % 4 == 0 { 0 } else { (rng % 1000) as i64 };
        reference[v][s][p] = q;
        let side = if s == 0 { Side::Buy } else { Side::Sell };
        g.set(v, side, PriceTicks(80 + p as i64), q, Timestamp(time))
            .unwrap();
        let lo = (rng >> 8) as usize % 64;
        let hi = (rng >> 16) as usize % 64;
        let (a, b) = (lo.min(hi), lo.max(hi));
        let expected = reference
            .iter()
            .enumerate()
            .map(|(v, r)| {
                r[s][a..=b]
                    .iter()
                    .map(|&q| i128::from(q) * [1_000_000, 750_000, 500_000][v])
                    .sum::<i128>()
            })
            .sum::<i128>();
        assert_eq!(g.depth(side, 80 + a as i128, 80 + b as i128), expected);
    }
    assert_eq!(
        g.depth(Side::Buy, i128::MIN, i128::MAX),
        g.depth(Side::Buy, 80, 143)
    );
    assert_eq!(g.depth(Side::Buy, i128::MAX, i128::MIN), 0);
}
#[test]
fn bucket_half_open_boundaries_match_direct_integer_inequalities() {
    let mut e = LiquidityEngine::<1, 64, 3>::new(cfg(), [1_000_000]).unwrap();
    for p in 80..144 {
        for side in [Side::Buy, Side::Sell] {
            e.update_level(0, side, PriceTicks(p), p, Timestamp(0))
                .unwrap();
        }
    }
    for mid in 161_i128..=285 {
        let buckets = e.buckets(mid, Timestamp(20)).unwrap();
        let mut lower = 0;
        for (i, &upper) in cfg().bucket_edges_ppm.iter().enumerate() {
            for side in [Side::Buy, Side::Sell] {
                let expected = (80_i128..144)
                    .filter(|&p| {
                        let distance = if side == Side::Buy {
                            mid - 2 * p
                        } else {
                            2 * p - mid
                        };
                        distance >= 0
                            && distance * 1_000_000 >= i128::from(lower) * mid
                            && distance * 1_000_000 < i128::from(upper) * mid
                    })
                    .map(|p| p * 1_000_000)
                    .sum::<i128>();
                assert_eq!(
                    if side == Side::Buy {
                        buckets[i].bid_depth
                    } else {
                        buckets[i].ask_depth
                    },
                    expected
                );
            }
            lower = upper;
        }
    }
}
#[test]
fn level_age_is_exact_and_clocks_cannot_regress() {
    let mut g = PriceGrid::<1, 8>::new(PriceTicks(i64::MAX - 7), 4, [1_000_000]).unwrap();
    g.set(0, Side::Buy, PriceTicks(i64::MAX), 1, Timestamp(2))
        .unwrap();
    g.set(0, Side::Buy, PriceTicks(i64::MAX - 1), 1, Timestamp(3))
        .unwrap();
    assert_eq!(
        g.age(Side::Buy, i128::MIN, i128::MAX, Timestamp(5))
            .unwrap(),
        (2, 5)
    );
    assert!(g.age(Side::Buy, 0, i128::MAX, Timestamp(1)).is_err());
    assert!(
        g.set(0, Side::Buy, PriceTicks(i64::MAX), 2, Timestamp(1))
            .is_err()
    );
}
#[test]
fn transparent_scores_require_enabled_data() {
    let c = ScoreComponents {
        depth_norm_ppm: 3_000_000,
        persistence_ppm: 1_000_000,
        stability_ppm: 500_000,
    };
    assert_eq!(pool_score(c, [6, 2, 2]).unwrap(), 2_100_000);
    assert_eq!(
        void_score(250_000, Some(500_000), Some(200_000), [2, 1, 1]).unwrap(),
        550_000
    );
    assert!(void_score(0, None, None, [1, 1, 0]).is_err());
    assert!(pool_score(c, [0, 0, 0]).is_err());
}
#[test]
fn baselines_warm_up_and_freeze_in_depleted_cells() {
    let mut e = LiquidityEngine::<1, 64, 3>::new(cfg(), [1_000_000]).unwrap();
    e.update_level(0, Side::Buy, PriceTicks(100), 100, Timestamp(0))
        .unwrap();
    for t in 0..3 {
        e.sample(Timestamp(t), &[1; 16], 250_000).unwrap();
        assert_eq!(e.cell(5).unwrap().depleted_mask, 0);
    }
    e.update_level(0, Side::Buy, PriceTicks(100), 0, Timestamp(3))
        .unwrap();
    for t in 3..100 {
        e.sample(Timestamp(t), &[1; 16], 250_000).unwrap();
        assert_eq!(e.cell(5).unwrap().depleted_mask, 1);
        assert_eq!(e.cell(5).unwrap().baseline, 100_000_000);
    }
    e.sample(Timestamp(100), &[0; 16], 250_000).unwrap();
    assert_eq!(e.cell(5).unwrap().depleted_mask, 0);
}

#[test]
fn coverage_cohort_change_restarts_pool_persistence() {
    let mut e = LiquidityEngine::<2, 64, 3>::new(cfg(), [1_000_000; 2]).unwrap();
    for v in 0..2 {
        e.update_level(v, Side::Buy, PriceTicks(100), 100, Timestamp(0))
            .unwrap();
    }
    for t in 0..3 {
        e.sample(Timestamp(t), &[3; 16], 250_000).unwrap();
    }
    for v in 0..2 {
        e.update_level(v, Side::Buy, PriceTicks(100), 500, Timestamp(3))
            .unwrap();
    }
    for t in 3..7 {
        e.sample(Timestamp(t), &[3; 16], 250_000).unwrap();
    }
    assert!(e.cell(5).unwrap().pool.is_some());
    e.sample(Timestamp(7), &[1; 16], 250_000).unwrap();
    assert!(e.cell(5).unwrap().pool.is_none());
    for t in 8..11 {
        e.sample(Timestamp(t), &[1; 16], 250_000).unwrap();
    }
    assert_eq!(e.cell(5).unwrap().pool.unwrap().persistent_ns, 3);
}
