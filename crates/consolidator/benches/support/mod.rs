use common::{InstrumentId, Side, Timestamp, VenueId};
use consolidator::{Consolidator, VenueConfig, normalization::*};
use fixed_point::{PriceTicks, QtyUnits};
use market_events::{MarketEvent, MarketEventType as K};
pub type Engine = Consolidator<3, 128, 384>;
pub fn setup() -> (Engine, u64, [u64; 3]) {
    let grid = InstrumentMetadata {
        instrument: InstrumentId(1),
        market: MarketIdentity {
            base_asset: 1,
            quote_asset: 2,
            settlement_asset: 2,
            kind: ContractKind::Spot,
            equivalence_group: 1,
        },
        price: UnitScale {
            atoms: 1,
            decimals: 2,
        },
        quantity: UnitScale {
            atoms: 1,
            decimals: 6,
        },
    };
    let configs = std::array::from_fn(|i| VenueConfig {
        venue: VenueId(i as u16 + 1),
        metadata: grid,
        weight_ppm: 1_000_000 - i as u32 * 250_000,
        stale_after_ns: u64::MAX,
    });
    let mut engine = Engine::new(grid, configs).unwrap();
    let mut now = 0;
    let mut sequences = [0; 3];
    for (i, sequence) in sequences.iter_mut().enumerate() {
        for index in 0..258 {
            now += 1;
            *sequence += 1;
            let (kind, side, price, qty) = match index {
                0 => (K::SnapshotStart, Side::Buy, 0, 0),
                257 => (K::SnapshotEnd, Side::Buy, 0, 0),
                n if n % 2 == 1 => (K::Add, Side::Buy, 10_000 - n / 2, 100),
                n => (K::Add, Side::Sell, 10_002 + n / 2, 100),
            };
            engine
                .apply(&MarketEvent {
                    venue: VenueId(i as u16 + 1),
                    instrument: grid.instrument,
                    sequence: *sequence,
                    exchange_sequence: *sequence,
                    exchange_ts: now,
                    receive_ts: Timestamp(now),
                    event_type: kind,
                    side,
                    price_ticks: PriceTicks(price),
                    qty_units: QtyUnits(qty),
                })
                .unwrap();
        }
    }
    (engine, now, sequences)
}
pub fn advance(now: &mut u64, sequences: &mut [u64; 3]) -> MarketEvent {
    *now += 1;
    let i = *now as usize % 3;
    sequences[i] += 1;
    MarketEvent {
        venue: VenueId(i as u16 + 1),
        instrument: InstrumentId(1),
        sequence: sequences[i],
        exchange_sequence: sequences[i],
        exchange_ts: *now,
        receive_ts: Timestamp(*now),
        event_type: K::Modify,
        side: Side::Buy,
        price_ticks: PriceTicks(10_000 - (*now % 128) as i64),
        qty_units: QtyUnits(100 + (*now % 19) as i64),
    }
}
