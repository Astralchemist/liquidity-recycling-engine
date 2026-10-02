//! Flow observation around the shared book/liquidity/void engine; no independent backtester.
use crate::*;
use book::{BookError, BookState, Level};
use common::{Side, Timestamp, VenueId};
use consolidator::{ConsolidationError, MarketState, VenueConfig, normalization::*};
use liquidity::{
    LiquidityConfig,
    research::{ResearchEngine, ResearchError},
};
use market_events::{MarketEvent, MarketEventType as K};
use voids::VoidConfig;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowResearchError {
    Research(ResearchError),
    Market(ConsolidationError),
    Book(BookError),
    Flow(FlowError),
    Faulted,
}
impl From<ResearchError> for FlowResearchError {
    fn from(e: ResearchError) -> Self {
        Self::Research(e)
    }
}
impl From<ConsolidationError> for FlowResearchError {
    fn from(e: ConsolidationError) -> Self {
        Self::Market(e)
    }
}
impl From<NormalizationError> for FlowResearchError {
    fn from(e: NormalizationError) -> Self {
        Self::Market(e.into())
    }
}
impl From<BookError> for FlowResearchError {
    fn from(e: BookError) -> Self {
        Self::Book(e)
    }
}
impl From<FlowError> for FlowResearchError {
    fn from(e: FlowError) -> Self {
        Self::Flow(e)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowResearchEngine<
    const V: usize,
    const N: usize,
    const G: usize,
    const P: usize,
    const B: usize,
    const Z: usize,
    const W: usize,
> {
    research: ResearchEngine<V, N, G, P, B, Z>,
    flows: [FlowEngine<W, B>; V],
    active: [bool; V],
    epochs: [u64; V],
    configs: [VenueConfig; V],
    normalizers: [Normalizer; V],
    config: FlowConfig,
    faulted: bool,
}
impl<
    const V: usize,
    const N: usize,
    const G: usize,
    const P: usize,
    const B: usize,
    const Z: usize,
    const W: usize,
> FlowResearchEngine<V, N, G, P, B, Z, W>
{
    pub fn new(
        grid: InstrumentMetadata,
        configs: [VenueConfig; V],
        liquidity: LiquidityConfig<B>,
        voids: VoidConfig,
        flow: FlowConfig,
    ) -> Result<Self, FlowResearchError> {
        if flow.qi_levels > N {
            return Err(FlowError::InvalidConfig.into());
        }
        FlowEngine::<W, B>::validate(flow)?;
        let research = ResearchEngine::new(grid, configs, liquidity, voids)?;
        Ok(Self {
            research,
            flows: std::array::from_fn(|_| {
                FlowEngine::new(flow, Timestamp(0)).expect("validated flow config")
            }),
            active: [false; V],
            epochs: [0; V],
            configs,
            normalizers: configs
                .map(|c| Normalizer::new(c.metadata, grid).expect("validated metadata")),
            config: flow,
            faulted: false,
        })
    }
    pub fn research(&self) -> &ResearchEngine<V, N, G, P, B, Z> {
        &self.research
    }
    pub fn faulted(&self) -> bool {
        self.faulted
    }
    fn healthy(&self) -> Result<(), FlowResearchError> {
        if self.faulted {
            Err(FlowResearchError::Faulted)
        } else {
            Ok(())
        }
    }
    fn index(&self, venue: VenueId) -> Result<usize, FlowResearchError> {
        self.configs
            .iter()
            .position(|c| c.venue == venue)
            .ok_or(ConsolidationError::UnknownVenue.into())
    }
    fn synchronize(&mut self, now: Timestamp) -> Result<(), FlowResearchError> {
        for v in 0..V {
            let book = self.research.market().venue_book(self.configs[v].venue)?;
            let active = self.configs[v].weight_ppm > 0
                && self.research.market().included(self.configs[v].venue)?
                && book.state() == BookState::Live
                && book.best(Side::Buy)?.is_some()
                && book.best(Side::Sell)?.is_some();
            if active != self.active[v] {
                self.flows[v].reset(now)?;
                self.active[v] = active;
                if active {
                    self.epochs[v] = self.epochs[v].checked_add(1).ok_or(FlowError::Overflow)?;
                }
            }
            if active {
                self.flows[v].advance_time(now)?;
            }
        }
        Ok(())
    }
    fn advance_inner(&mut self, now: Timestamp) -> Result<(), FlowResearchError> {
        self.research.advance_time(now)?;
        self.synchronize(now)
    }
    pub fn advance_time(&mut self, now: Timestamp) -> Result<(), FlowResearchError> {
        self.healthy()?;
        let result = self.advance_inner(now);
        if result.is_err() {
            self.faulted = true;
        }
        result
    }
    fn best(&self, v: usize) -> Result<BestQuotes, FlowResearchError> {
        let book = self.research.market().venue_book(self.configs[v].venue)?;
        let normalize = |l: Level| -> Result<Level, NormalizationError> {
            Ok(Level {
                price: self.normalizers[v].price(l.price)?,
                qty: self.normalizers[v].quantity(l.qty)?,
            })
        };
        Ok(BestQuotes {
            bid: normalize(book.best(Side::Buy)?.ok_or(FlowError::InvalidObservation)?)?,
            ask: normalize(
                book.best(Side::Sell)?
                    .ok_or(FlowError::InvalidObservation)?,
            )?,
        })
    }
    fn pre_event_midpoint(&self, now: Timestamp) -> Result<Option<i128>, FlowResearchError> {
        let (mut bid, mut ask): (Option<i64>, Option<i64>) = (None, None);
        for v in 0..V {
            if self.configs[v].weight_ppm == 0
                || !self
                    .research
                    .market()
                    .fresh_at(self.configs[v].venue, now)?
            {
                continue;
            }
            let book = self.research.market().venue_book(self.configs[v].venue)?;
            if let Some(b) = book.best(Side::Buy)? {
                let p = self.normalizers[v].price(b.price)?.0;
                bid = Some(bid.map_or(p, |old| old.max(p)));
            }
            if let Some(a) = book.best(Side::Sell)? {
                let p = self.normalizers[v].price(a.price)?.0;
                ask = Some(ask.map_or(p, |old| old.min(p)));
            }
        }
        Ok(match (bid, ask) {
            (Some(b), Some(a)) if b < a => Some(i128::from(b) + i128::from(a)),
            _ => None,
        })
    }
    pub fn apply(&mut self, event: &MarketEvent) -> Result<(), FlowResearchError> {
        self.healthy()?;
        let result = self.apply_inner(event);
        if result.is_err() {
            self.faulted = true;
        }
        result
    }
    fn apply_inner(&mut self, e: &MarketEvent) -> Result<(), FlowResearchError> {
        let v = self.index(e.venue)?;
        let was_fresh = self.active[v] && self.research.market().fresh_at(e.venue, e.receive_ts)?;
        let before = if was_fresh { Some(self.best(v)?) } else { None };
        let depth_event = matches!(e.event_type, K::Add | K::Modify | K::Cancel);
        let old = if self.active[v] && depth_event {
            self.research
                .market()
                .venue_book(e.venue)?
                .level(e.side, e.price_ticks)?
                .map_or(0, |l| l.qty.0)
        } else {
            0
        };
        let midpoint = self.pre_event_midpoint(e.receive_ts)?;
        self.research.apply(e)?;
        self.synchronize(e.receive_ts)?;
        let Some(before) = before else { return Ok(()) };
        if !self.active[v] || matches!(e.event_type, K::SnapshotStart | K::SnapshotEnd) {
            return Ok(());
        }
        let contribution = if depth_event {
            let new = if e.event_type == K::Cancel {
                0
            } else {
                e.qty_units.0
            };
            let delta = i128::from(self.normalizers[v].quantity(fixed_point::QtyUnits(new))?.0)
                - i128::from(self.normalizers[v].quantity(fixed_point::QtyUnits(old))?.0);
            Contribution::depth(
                best_quote_ofi(before, self.best(v)?)?,
                e.side,
                delta,
                self.config.removal_attribution,
            )?
        } else {
            // Trades are reported separately; their later depth reduction supplies OFI.
            Contribution::trade(e.side, self.normalizers[v].quantity(e.qty_units)?.0)?
        };
        let resting_side = if e.event_type == K::Trade {
            match e.side {
                Side::Buy => Side::Sell,
                Side::Sell => Side::Buy,
            }
        } else {
            e.side
        };
        let price = self.normalizers[v].price(e.price_ticks)?.0;
        let bucket = midpoint.and_then(|mid| {
            distance_bucket(
                &self.research.liquidity().config().bucket_edges_ppm,
                mid,
                price,
                resting_side,
            )
        });
        self.flows[v].record(e.receive_ts, contribution, bucket)?;
        Ok(())
    }
    pub fn snapshot(&self, venue: VenueId) -> Result<Option<FlowSnapshot<B>>, FlowResearchError> {
        self.healthy()?;
        let v = self.index(venue)?;
        if !self.active[v] {
            return Ok(None);
        }
        Ok(Some(self.flows[v].snapshot()?))
    }
    pub fn epoch(&self, venue: VenueId) -> Result<u64, FlowResearchError> {
        self.healthy()?;
        Ok(self.epochs[self.index(venue)?])
    }
    /// Current per-venue QI; both depths use the same native scale, which cancels.
    pub fn queue_imbalance(
        &self,
        venue: VenueId,
        best_only: bool,
    ) -> Result<Option<i32>, FlowResearchError> {
        self.healthy()?;
        let v = self.index(venue)?;
        if !self.active[v] {
            return Ok(None);
        }
        let book = self.research.market().venue_book(venue)?;
        let n = if best_only { 1 } else { self.config.qi_levels };
        let depth = |side| -> Result<i128, BookError> {
            Ok(book
                .levels(side)?
                .iter()
                .take(n)
                .map(|l| i128::from(l.qty.0))
                .sum())
        };
        Ok(queue_imbalance(depth(Side::Buy)?, depth(Side::Sell)?)?)
    }
    pub fn bucket_queue_imbalance(
        &self,
        now: Timestamp,
    ) -> Result<[Option<i32>; B], FlowResearchError> {
        self.healthy()?;
        if self.research.market().market_state()? != MarketState::Open {
            return Ok([None; B]);
        }
        let midpoint = self
            .research
            .market()
            .midpoint_x2()?
            .ok_or(FlowError::InvalidObservation)?;
        let buckets = self
            .research
            .liquidity()
            .buckets(midpoint, now)
            .map_err(ResearchError::from)?;
        let mut result = [None; B];
        for (q, b) in result.iter_mut().zip(buckets) {
            if b.complete_corridor {
                *q = queue_imbalance(b.bid_depth, b.ask_depth)?;
            }
        }
        Ok(result)
    }
    /// Weighted per-venue time-window OFI, in common quantity * ppm, with source mask.
    /// Source epochs/exposure can differ; this is not consolidated-best-quote OFI.
    pub fn weighted_time_ofi(&self) -> Result<(i128, u64), FlowResearchError> {
        self.healthy()?;
        let (mut value, mut mask) = (0, 0);
        for v in 0..V {
            if self.active[v] {
                value += self.flows[v].snapshot()?.time_totals.get(Metric::Ofi)
                    * i128::from(self.configs[v].weight_ppm);
                mask |= 1 << v;
            }
        }
        Ok((value, mask))
    }
}
