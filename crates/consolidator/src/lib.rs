//! Incremental weighted depth over independent venue books. Single owner, bounded memory.
#![forbid(unsafe_code)]
mod ladder;
pub mod normalization;
use book::{BookError, BookState, VenueBook};
use common::{Side, Timestamp, VenueId};
use fixed_point::{BasisPoints, QtyUnits};
pub use ladder::ConsolidatedLevel;
use ladder::Ladder;
use market_events::{MarketEvent, MarketEventType as K};
use normalization::{InstrumentMetadata, NormalizationError, Normalizer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueConfig {
    pub venue: VenueId,
    pub metadata: InstrumentMetadata,
    pub weight_ppm: u32,
    pub stale_after_ns: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationError {
    UnknownVenue,
    DuplicateVenue,
    InvalidConfig,
    WrongMarket,
    Capacity,
    Overflow,
    Invariant,
    ClockRegression,
    Faulted,
    Normalization(NormalizationError),
    Book { venue: VenueId, error: BookError },
}
impl From<NormalizationError> for ConsolidationError {
    fn from(e: NormalizationError) -> Self {
        Self::Normalization(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketState {
    Empty,
    OneSided,
    Open,
    Locked,
    Crossed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divergence {
    pub midpoint_difference_x2: i128,
    pub basis_points: BasisPoints,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepthDisagreement {
    pub venues: usize,
    /// Exact population variance = numerator / denominator, in common quantity units squared.
    pub variance_numerator: i128,
    pub variance_denominator: i128,
    pub sigma_units_floor: u128,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Venue<const N: usize> {
    config: VenueConfig,
    normalizer: Normalizer,
    book: VenueBook<N>,
    included: bool,
    last_depth: Option<Timestamp>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consolidator<const V: usize, const N: usize, const G: usize> {
    venues: [Venue<N>; V],
    grid: InstrumentMetadata,
    bids: Ladder<G>,
    asks: Ladder<G>,
    now: Option<Timestamp>,
    fault: Option<ConsolidationError>,
    structural_revision: u64,
}
impl<const V: usize, const N: usize, const G: usize> Consolidator<V, N, G> {
    pub fn new(
        grid: InstrumentMetadata,
        configs: [VenueConfig; V],
    ) -> Result<Self, ConsolidationError> {
        if V == 0 || N == 0 || G < V.checked_mul(N).ok_or(ConsolidationError::InvalidConfig)? {
            return Err(ConsolidationError::InvalidConfig);
        }
        Normalizer::new(grid, grid)?;
        for (i, c) in configs.iter().enumerate() {
            if c.stale_after_ns == 0 || c.weight_ppm > 1_000_000 {
                return Err(ConsolidationError::InvalidConfig);
            }
            if configs[..i].iter().any(|p| p.venue == c.venue) {
                return Err(ConsolidationError::DuplicateVenue);
            }
            Normalizer::new(c.metadata, grid)?;
        }
        let venues = configs.map(|config| Venue {
            normalizer: Normalizer::new(config.metadata, grid).expect("validated"),
            book: VenueBook::new(config.venue, config.metadata.instrument),
            config,
            included: false,
            last_depth: None,
        });
        Ok(Self {
            venues,
            grid,
            bids: Ladder::new(Side::Buy),
            asks: Ladder::new(Side::Sell),
            now: None,
            fault: None,
            structural_revision: 0,
        })
    }
    fn healthy(&self) -> Result<(), ConsolidationError> {
        if self.fault.is_some() {
            Err(ConsolidationError::Faulted)
        } else {
            Ok(())
        }
    }
    pub fn fault(&self) -> Option<ConsolidationError> {
        self.fault
    }
    /// Changes on venue activation/withdrawal or atomic-batch replacement, not single-level updates.
    pub fn structural_revision(&self) -> u64 {
        self.structural_revision
    }
    pub fn grid(&self) -> InstrumentMetadata {
        self.grid
    }
    pub fn venue_config(&self, venue: VenueId) -> Result<VenueConfig, ConsolidationError> {
        Ok(self.venues[self.index(venue)?].config)
    }
    fn index(&self, venue: VenueId) -> Result<usize, ConsolidationError> {
        self.venues
            .iter()
            .position(|v| v.config.venue == venue)
            .ok_or(ConsolidationError::UnknownVenue)
    }
    pub fn venue_book(&self, venue: VenueId) -> Result<&VenueBook<N>, ConsolidationError> {
        Ok(&self.venues[self.index(venue)?].book)
    }
    /// Nonmutating freshness query for pre-event measurements. Uses depth time,
    /// not trade/receive time. The subsequent apply/advance still performs withdrawal.
    pub fn fresh_at(&self, venue: VenueId, now: Timestamp) -> Result<bool, ConsolidationError> {
        self.healthy()?;
        if self.now.is_some_and(|t| now < t) {
            return Err(ConsolidationError::ClockRegression);
        }
        let v = &self.venues[self.index(venue)?];
        Ok(v.included
            && v.book.state() == BookState::Live
            && v.last_depth
                .is_some_and(|t| now.0 - t.0 <= v.config.stale_after_ns))
    }
    pub fn included(&self, venue: VenueId) -> Result<bool, ConsolidationError> {
        self.healthy()?;
        Ok(self.venues[self.index(venue)?].included)
    }
    pub fn levels(&self, side: Side) -> Result<&[ConsolidatedLevel], ConsolidationError> {
        self.healthy()?;
        Ok(match side {
            Side::Buy => self.bids.levels(),
            Side::Sell => self.asks.levels(),
        })
    }
    pub fn depth(&self, side: Side) -> Result<i128, ConsolidationError> {
        self.healthy()?;
        Ok(match side {
            Side::Buy => self.bids.total(),
            Side::Sell => self.asks.total(),
        })
    }
    /// Explicit research query (O(V*N)); never invoked implicitly on each event.
    /// Missing depth is zero; stale and zero-weight venues are excluded. No bucket inference.
    pub fn depth_disagreement(
        &self,
        side: Side,
        price: fixed_point::PriceTicks,
    ) -> Result<Option<DepthDisagreement>, ConsolidationError> {
        self.healthy()?;
        let (mut count, mut sum, mut squares) = (0_usize, 0_i128, 0_i128);
        for v in &self.venues {
            if !v.included || v.config.weight_ppm == 0 {
                continue;
            }
            let mut quantity = 0_i128;
            for level in v.book.last_committed_levels(side) {
                if v.normalizer.price(level.price)? == price {
                    quantity = i128::from(v.normalizer.quantity(level.qty)?.0);
                    break;
                }
            }
            count += 1;
            sum = sum
                .checked_add(quantity)
                .ok_or(ConsolidationError::Overflow)?;
            squares = squares
                .checked_add(
                    quantity
                        .checked_mul(quantity)
                        .ok_or(ConsolidationError::Overflow)?,
                )
                .ok_or(ConsolidationError::Overflow)?;
        }
        if count == 0 {
            return Ok(None);
        }
        let n = count as i128;
        let numerator = n
            .checked_mul(squares)
            .and_then(|x| sum.checked_mul(sum).and_then(|s| x.checked_sub(s)))
            .ok_or(ConsolidationError::Overflow)?;
        let denominator = n.checked_mul(n).ok_or(ConsolidationError::Overflow)?;
        let value = (numerator / denominator) as u128;
        let (mut low, mut high) = (0_u128, 1_u128 << 64);
        while low + 1 < high {
            let mid = (low + high) / 2;
            if mid <= value / mid {
                low = mid;
            } else {
                high = mid;
            }
        }
        Ok(Some(DepthDisagreement {
            venues: count,
            variance_numerator: numerator,
            variance_denominator: denominator,
            sigma_units_floor: low,
        }))
    }
    pub fn midpoint_x2(&self) -> Result<Option<i128>, ConsolidationError> {
        Ok(
            match (
                self.levels(Side::Buy)?.first(),
                self.levels(Side::Sell)?.first(),
            ) {
                (Some(b), Some(a)) => Some(i128::from(b.price.0) + i128::from(a.price.0)),
                _ => None,
            },
        )
    }
    pub fn spread(&self) -> Result<Option<i64>, ConsolidationError> {
        Ok(
            match (
                self.levels(Side::Buy)?.first(),
                self.levels(Side::Sell)?.first(),
            ) {
                (Some(b), Some(a)) => Some(a.price.0 - b.price.0),
                _ => None,
            },
        )
    }
    pub fn market_state(&self) -> Result<MarketState, ConsolidationError> {
        Ok(match self.spread()? {
            Some(s) if s < 0 => MarketState::Crossed,
            Some(0) => MarketState::Locked,
            Some(_) => MarketState::Open,
            None if self.levels(Side::Buy)?.is_empty() && self.levels(Side::Sell)?.is_empty() => {
                MarketState::Empty
            }
            None => MarketState::OneSided,
        })
    }
    pub fn venue_midpoint_x2(&self, venue: VenueId) -> Result<Option<i128>, ConsolidationError> {
        self.healthy()?;
        let v = &self.venues[self.index(venue)?];
        if !v.included {
            return Ok(None);
        }
        let (Some(b), Some(a)) = (
            v.book
                .best(Side::Buy)
                .map_err(|_| ConsolidationError::Invariant)?,
            v.book
                .best(Side::Sell)
                .map_err(|_| ConsolidationError::Invariant)?,
        ) else {
            return Ok(None);
        };
        Ok(Some(
            i128::from(v.normalizer.price(b.price)?.0) + i128::from(v.normalizer.price(a.price)?.0),
        ))
    }
    pub fn divergence(&self, venue: VenueId) -> Result<Option<Divergence>, ConsolidationError> {
        let (Some(local), Some(global)) = (self.venue_midpoint_x2(venue)?, self.midpoint_x2()?)
        else {
            return Ok(None);
        };
        let difference = local - global;
        let bps = difference
            .checked_mul(10_000 * BasisPoints::SCALE)
            .ok_or(ConsolidationError::Overflow)?
            / global;
        Ok(Some(Divergence {
            midpoint_difference_x2: difference,
            basis_points: BasisPoints(
                i64::try_from(bps).map_err(|_| ConsolidationError::Overflow)?,
            ),
        }))
    }
    fn change(
        &mut self,
        side: Side,
        price: fixed_point::PriceTicks,
        delta: i128,
    ) -> Result<(), ConsolidationError> {
        match side {
            Side::Buy => self.bids.change(price, delta),
            Side::Sell => self.asks.change(price, delta),
        }
    }
    // Only activation/removal traverses one venue. Ordinary changes adjust a single price.
    fn contribution(&mut self, index: usize, sign: i128) -> Result<(), ConsolidationError> {
        self.structural_revision = self
            .structural_revision
            .checked_add(1)
            .ok_or(ConsolidationError::Overflow)?;
        for side in [Side::Buy, Side::Sell] {
            let count = self.venues[index].book.last_committed_levels(side).len();
            for j in 0..count {
                let v = &self.venues[index];
                let l = v.book.last_committed_levels(side)[j];
                let price = v.normalizer.price(l.price)?;
                let qty = i128::from(v.normalizer.quantity(l.qty)?.0)
                    * i128::from(v.config.weight_ppm)
                    * sign;
                self.change(side, price, qty)?;
            }
        }
        Ok(())
    }
    fn exclude(&mut self, index: usize) -> Result<(), ConsolidationError> {
        if self.venues[index].included {
            self.contribution(index, -1)?;
            self.venues[index].included = false;
        }
        Ok(())
    }
    fn fatal<T>(&mut self, result: Result<T, ConsolidationError>) -> Result<T, ConsolidationError> {
        if let Err(e) = result {
            self.fault = Some(e);
        }
        result
    }
    pub fn advance_time(&mut self, now: Timestamp) -> Result<(), ConsolidationError> {
        self.healthy()?;
        let result = self.advance_inner(now);
        self.fatal(result)
    }
    fn advance_inner(&mut self, now: Timestamp) -> Result<(), ConsolidationError> {
        if self.now.is_some_and(|t| now < t) {
            return Err(ConsolidationError::ClockRegression);
        }
        self.now = Some(now);
        for i in 0..V {
            let v = &self.venues[i];
            if v.book.state() == BookState::Live
                && v.last_depth
                    .is_some_and(|t| now.0 - t.0 > v.config.stale_after_ns)
            {
                self.exclude(i)?;
                self.venues[i].book.invalidate(BookError::Stale);
            }
        }
        Ok(())
    }
    pub fn disconnect(&mut self, venue: VenueId, now: Timestamp) -> Result<(), ConsolidationError> {
        let i = self.index(venue)?;
        self.advance_time(now)?;
        let result = self.exclude(i);
        self.fatal(result)?;
        self.venues[i].book.invalidate(BookError::Disconnected);
        Ok(())
    }
    pub fn apply(&mut self, e: &MarketEvent) -> Result<(), ConsolidationError> {
        self.healthy()?;
        let i = self.index(e.venue)?;
        if e.instrument != self.venues[i].config.metadata.instrument {
            return Err(ConsolidationError::WrongMarket);
        }
        self.advance_time(e.receive_ts)?;
        // For failure isolation, remove a venue BEFORE its book can become unreadable.
        // Save only one old level on the normal path; full withdrawal happens only on errors.
        let old = if self.venues[i].included && matches!(e.event_type, K::Modify | K::Cancel) {
            self.venues[i]
                .book
                .level(e.side, e.price_ticks)
                .map_err(|_| ConsolidationError::Invariant)?
                .map(|l| l.qty)
                .unwrap_or(QtyUnits(0))
        } else {
            QtyUnits(0)
        };
        if e.event_type == K::SnapshotStart {
            let result = self.exclude(i);
            self.fatal(result)?;
        }
        // Validate normalization before changing either state. A bad scale is a global fault.
        let normalized = if matches!(e.event_type, K::Add | K::Modify | K::Cancel | K::Trade) {
            let n = self.venues[i].normalizer;
            let result = (|| {
                Ok((
                    n.price(e.price_ticks)?,
                    n.quantity(e.qty_units)?,
                    n.quantity(old)?,
                ))
            })();
            Some(self.fatal(result)?)
        } else {
            None
        };
        // Failed single updates retain committed depth, available only for audit/withdrawal.
        if let Err(error) = self.venues[i].book.apply(e) {
            if self.venues[i].included {
                let result = self.exclude(i);
                self.fatal(result)?;
                self.venues[i].book.invalidate(error);
            }
            return Err(ConsolidationError::Book {
                venue: e.venue,
                error,
            });
        }
        if matches!(
            e.event_type,
            K::Add | K::Modify | K::Cancel | K::SnapshotEnd
        ) {
            self.venues[i].last_depth = Some(e.receive_ts);
        }
        if self.venues[i].book.state() == BookState::Live {
            let result = if !self.venues[i].included {
                self.contribution(i, 1)
            } else if let Some((price, qty, old)) = normalized {
                if e.event_type == K::Trade {
                    Ok(())
                } else {
                    self.change(
                        e.side,
                        price,
                        (i128::from(qty.0) - i128::from(old.0))
                            * i128::from(self.venues[i].config.weight_ppm),
                    )
                }
            } else {
                Ok(())
            };
            self.fatal(result)?;
            self.venues[i].included = true;
        }
        Ok(())
    }
    /// Rare atomic message path: withdraw/reinsert one venue, never all venues.
    pub fn apply_depth_batch(
        &mut self,
        venue: VenueId,
        events: &[MarketEvent],
    ) -> Result<(), ConsolidationError> {
        self.healthy()?;
        let i = self.index(venue)?;
        let first = events.first().ok_or(ConsolidationError::InvalidConfig)?;
        if events
            .iter()
            .any(|e| e.venue != venue || e.instrument != self.venues[i].config.metadata.instrument)
        {
            return Err(ConsolidationError::WrongMarket);
        }
        self.advance_time(first.receive_ts)?;
        let result = self.exclude(i);
        self.fatal(result)?;
        self.venues[i]
            .book
            .apply_depth_batch(events)
            .map_err(|error| ConsolidationError::Book { venue, error })?;
        self.venues[i].last_depth = Some(first.receive_ts);
        let result = self.contribution(i, 1);
        self.fatal(result)?;
        self.venues[i].included = true;
        Ok(())
    }
}
