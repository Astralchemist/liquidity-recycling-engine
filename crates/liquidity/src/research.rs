//! Shared event path for direct input and recorded replay. No file I/O in this module.
use crate::{LiquidityConfig, LiquidityEngine, LiquidityError, PriceReference, void_score};
use common::{Side, Timestamp};
use consolidator::{
    ConsolidationError, Consolidator, MarketState, VenueConfig,
    normalization::{InstrumentMetadata, Normalizer},
};
use market_events::{MarketEvent, MarketEventType as K};
use voids::{FormationEvidence, VoidConfig, VoidEngine, VoidError};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchError {
    Market(ConsolidationError),
    Liquidity(LiquidityError),
    Voids(VoidError),
    Faulted,
}
impl From<ConsolidationError> for ResearchError {
    fn from(e: ConsolidationError) -> Self {
        Self::Market(e)
    }
}
impl From<LiquidityError> for ResearchError {
    fn from(e: LiquidityError) -> Self {
        Self::Liquidity(e)
    }
}
impl From<VoidError> for ResearchError {
    fn from(e: VoidError) -> Self {
        Self::Voids(e)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchEngine<
    const V: usize,
    const N: usize,
    const G: usize,
    const P: usize,
    const B: usize,
    const Z: usize,
> {
    market: Consolidator<V, N, G>,
    liquidity: LiquidityEngine<V, P, B>,
    voids: VoidEngine<Z>,
    configs: [VenueConfig; V],
    normalizers: [Normalizer; V],
    revision: u64,
    last_trade_x2: Option<i128>,
    faulted: bool,
    coverage: [u64; P],
    bounds: [Option<voids::PriceRegion>; V],
}
impl<const V: usize, const N: usize, const G: usize, const P: usize, const B: usize, const Z: usize>
    ResearchEngine<V, N, G, P, B, Z>
{
    pub fn new(
        grid: InstrumentMetadata,
        configs: [VenueConfig; V],
        liquidity: LiquidityConfig<B>,
        voids: VoidConfig,
    ) -> Result<Self, ResearchError> {
        let market = Consolidator::new(grid, configs)?;
        let normalizers = configs
            .map(|c| Normalizer::new(c.metadata, grid).expect("market configuration validated"));
        let liquidity = LiquidityEngine::new(liquidity, configs.map(|c| c.weight_ppm))?;
        if liquidity.config().cell_width as i64 - 1 < voids.minimum_width_ticks {
            return Err(LiquidityError::InvalidConfig.into());
        }
        Ok(Self {
            market,
            liquidity,
            voids: VoidEngine::new(voids)?,
            configs,
            normalizers,
            revision: 0,
            last_trade_x2: None,
            faulted: false,
            coverage: [0; P],
            bounds: [None; V],
        })
    }
    pub fn market(&self) -> &Consolidator<V, N, G> {
        &self.market
    }
    pub fn liquidity(&self) -> &LiquidityEngine<V, P, B> {
        &self.liquidity
    }
    pub fn voids(&self) -> &VoidEngine<Z> {
        &self.voids
    }
    pub fn faulted(&self) -> bool {
        self.faulted
    }
    /// The configured reference midpoint and spread (doubled ticks, ticks). Midpoint and
    /// last-trade research use the consolidated touch, defined only while it is open;
    /// composite research uses valid venues even when the consolidated touch is crossed.
    pub fn reference(&self) -> Result<(Option<i128>, Option<i64>), ResearchError> {
        Ok(match self.liquidity.config().price_reference {
            PriceReference::Composite => (
                self.market.composite_midpoint_x2(None)?,
                self.market.composite_spread()?,
            ),
            _ if self.market.market_state()? == MarketState::Open => {
                (self.market.midpoint_x2()?, self.market.spread()?)
            }
            _ => (None, None),
        })
    }
    pub fn apply(&mut self, event: &MarketEvent) -> Result<(), ResearchError> {
        if self.faulted {
            return Err(ResearchError::Faulted);
        }
        let result = (|| {
            self.market.apply(event)?;
            self.observe(event.receive_ts, std::slice::from_ref(event))
        })();
        if result.is_err() {
            self.faulted = true;
        }
        result
    }
    /// One atomic native depth message: every changed grid level is updated, then formation
    /// sampling and zone observation run ONCE against the committed post-batch state.
    pub fn apply_depth_batch(
        &mut self,
        venue: common::VenueId,
        events: &[MarketEvent],
    ) -> Result<(), ResearchError> {
        if self.faulted {
            return Err(ResearchError::Faulted);
        }
        let result = (|| {
            self.market.apply_depth_batch(venue, events)?;
            let now = events
                .first()
                .ok_or(ConsolidationError::InvalidConfig)?
                .receive_ts;
            self.observe(now, events)
        })();
        if result.is_err() {
            self.faulted = true;
        }
        result
    }
    pub fn advance_time(&mut self, now: Timestamp) -> Result<(), ResearchError> {
        if self.faulted {
            return Err(ResearchError::Faulted);
        }
        let result = (|| {
            self.market.advance_time(now)?;
            self.observe(now, &[])
        })();
        if result.is_err() {
            self.faulted = true;
        }
        result
    }
    fn rebuild(&mut self, now: Timestamp) -> Result<(), ResearchError> {
        self.liquidity.reset()?;
        self.voids.invalidate_all(now)?;
        self.last_trade_x2 = None;
        for (v, c) in self.configs.iter().enumerate() {
            if !self.market.included(c.venue)? {
                continue;
            }
            let book = self.market.venue_book(c.venue)?;
            for side in [Side::Buy, Side::Sell] {
                for level in book.last_committed_levels(side) {
                    let price = self.normalizers[v]
                        .price(level.price)
                        .map_err(ConsolidationError::from)?;
                    let qty = self.normalizers[v]
                        .quantity(level.qty)
                        .map_err(ConsolidationError::from)?;
                    self.liquidity.grid.set(v, side, price, qty.0, now)?;
                }
            }
        }
        Ok(())
    }
    fn observe(&mut self, now: Timestamp, events: &[MarketEvent]) -> Result<(), ResearchError> {
        if self.revision != self.market.structural_revision() {
            self.rebuild(now)?;
            self.revision = self.market.structural_revision();
        } else {
            for e in events {
                if !matches!(e.event_type, K::Add | K::Modify | K::Cancel) {
                    continue;
                }
                let v = self
                    .configs
                    .iter()
                    .position(|c| c.venue == e.venue)
                    .ok_or(ConsolidationError::UnknownVenue)?;
                if self.market.included(e.venue)? {
                    let qty = self
                        .market
                        .venue_book(e.venue)?
                        .level(e.side, e.price_ticks)
                        .map_err(|error| ConsolidationError::Book {
                            venue: e.venue,
                            error,
                        })?
                        .map_or(0, |l| l.qty.0);
                    let price = self.normalizers[v]
                        .price(e.price_ticks)
                        .map_err(ConsolidationError::from)?;
                    let qty = self.normalizers[v]
                        .quantity(fixed_point::QtyUnits(qty))
                        .map_err(ConsolidationError::from)?;
                    self.liquidity.grid.set(v, e.side, price, qty.0, now)?;
                }
            }
        }
        for e in events {
            if e.event_type == K::Trade {
                let v = self
                    .configs
                    .iter()
                    .position(|c| c.venue == e.venue)
                    .ok_or(ConsolidationError::UnknownVenue)?;
                if self.configs[v].weight_ppm > 0 && self.market.included(e.venue)? {
                    self.last_trade_x2 = Some(
                        i128::from(
                            self.normalizers[v]
                                .price(e.price_ticks)
                                .map_err(ConsolidationError::from)?
                                .0,
                        ) * 2,
                    );
                }
            }
        }
        let (reference, _) = self.reference()?;
        let usable = match self.liquidity.config().price_reference {
            PriceReference::Composite => reference.is_some(),
            _ => self.market.market_state() == Ok(MarketState::Open),
        };
        if !usable {
            self.voids.invalidate_all(now)?;
            self.liquidity.suspend();
            return Ok(());
        }
        let price = match self.liquidity.config().price_reference {
            PriceReference::LastTrade => self.last_trade_x2,
            _ => reference,
        };
        let Some(price) = price else { return Ok(()) };
        let mut eligible = 0_u64;
        if self.liquidity.config().assume_contiguous_l2_coverage {
            for (v, c) in self.configs.iter().enumerate() {
                if c.weight_ppm == 0 {
                    continue;
                }
                eligible |= 1 << v;
                let book = self.market.venue_book(c.venue)?;
                let bounds = if self.market.included(c.venue)? {
                    match (
                        book.last_committed_levels(Side::Buy).last(),
                        book.last_committed_levels(Side::Sell).last(),
                    ) {
                        (Some(bid), Some(ask)) => Some(voids::PriceRegion {
                            lower: self.normalizers[v]
                                .price(bid.price)
                                .map_err(ConsolidationError::from)?,
                            upper: self.normalizers[v]
                                .price(ask.price)
                                .map_err(ConsolidationError::from)?,
                        }),
                        _ => None,
                    }
                } else {
                    None
                };
                if self.bounds[v] == bounds {
                    continue;
                }
                self.bounds[v] = bounds;
                for (i, mask) in self.coverage[..self.liquidity.cell_count()]
                    .iter_mut()
                    .enumerate()
                {
                    let r = self.liquidity.region(i);
                    *mask &= !(1 << v);
                    if bounds.is_some_and(|b| b.lower <= r.lower && b.upper >= r.upper) {
                        *mask |= 1 << v;
                    }
                }
            }
        }
        if self.liquidity.due(now) {
            self.liquidity.sample(
                now,
                &self.coverage[..self.liquidity.cell_count()],
                self.voids.config().low_depth_ppm,
            )?;
            for i in 0..self.liquidity.cell_count() {
                let obs = self.liquidity.cell(i).expect("valid cell");
                if obs.depleted_mask == 0 {
                    continue;
                }
                let (depth, baseline) = self.liquidity.evidence(i, obs.depleted_mask);
                let ratio = (depth * crate::PPM / baseline.max(1)).min(i128::from(u32::MAX)) as u32;
                self.voids.consider(
                    now,
                    self.liquidity.region(i),
                    FormationEvidence {
                        depth,
                        baseline,
                        score_ppm: void_score(ratio, None, None, [1, 0, 0])?,
                        venue_mask: obs.depleted_mask,
                        eligible_mask: eligible,
                    },
                    price,
                )?;
            }
        }
        for slot in 0..Z {
            let Some(zone) = self.voids.zones()[slot] else {
                continue;
            };
            if !zone.state.active() {
                continue;
            }
            let cell = ((zone.region.lower.0 - self.liquidity.config().lower_price.0) as usize)
                / self.liquidity.config().cell_width;
            let (depth, _) = self.liquidity.evidence(cell, zone.venue_mask);
            let covered = self.coverage[cell] & zone.venue_mask == zone.venue_mask
                && self.coverage[cell].count_ones()
                    >= self.liquidity.config().minimum_covered_venues;
            self.voids.observe(slot, now, price, depth, covered)?;
        }
        Ok(())
    }
}
