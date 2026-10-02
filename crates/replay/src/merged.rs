//! Bounded k-way merge within one explicitly shared local monotonic clock domain.
use common::{Side, VenueId};
use consolidator::{ConsolidationError, Consolidator};
use market_events::MarketEvent;
use recorder::{RecordingMetadata, RecordingReader};
use std::io::{self, Read};
#[derive(Debug)]
pub enum MergeError {
    Io(io::Error),
    DuplicateVenue,
    MetadataMismatch,
    ClockDomainRequired,
    Consolidation(ConsolidationError),
    IncompleteSnapshot,
}
impl From<io::Error> for MergeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<ConsolidationError> for MergeError {
    fn from(e: ConsolidationError) -> Self {
        Self::Consolidation(e)
    }
}
pub struct MergedReplay<R, const V: usize> {
    readers: [RecordingReader<R>; V],
    heads: [Option<MarketEvent>; V],
    refill: Option<usize>,
    failed: bool,
}
impl<R: Read, const V: usize> MergedReplay<R, V> {
    /// v1 has no clock-domain metadata. Explicit attestation is mandatory.
    pub fn new(inputs: [R; V], shared_clock_domain: bool) -> Result<Self, MergeError> {
        if !shared_clock_domain || V == 0 {
            return Err(MergeError::ClockDomainRequired);
        }
        // Initialization only: no allocations while merging or applying events.
        let readers: Vec<_> = inputs
            .into_iter()
            .map(RecordingReader::new)
            .collect::<io::Result<_>>()?;
        let mut readers: [RecordingReader<R>; V] = readers
            .try_into()
            .map_err(|_| MergeError::MetadataMismatch)?;
        for i in 0..V {
            if readers[..i]
                .iter()
                .any(|r| r.metadata.venue == readers[i].metadata.venue)
            {
                return Err(MergeError::DuplicateVenue);
            }
        }
        let mut heads = [None; V];
        for i in 0..V {
            heads[i] = readers[i].next_event()?;
        }
        Ok(Self {
            readers,
            heads,
            refill: None,
            failed: false,
        })
    }
    pub fn metadata(&self) -> [RecordingMetadata; V] {
        std::array::from_fn(|i| self.readers[i].metadata)
    }
    fn next_inner(&mut self) -> Result<Option<MarketEvent>, MergeError> {
        if let Some(i) = self.refill.take() {
            self.heads[i] = self.readers[i].next_event()?;
        }
        let i = (0..V)
            .filter(|&i| self.heads[i].is_some())
            .min_by_key(|&i| {
                let e = self.heads[i].expect("filtered");
                (e.receive_ts.0, e.venue.0, e.sequence)
            });
        let Some(i) = i else { return Ok(None) };
        self.refill = Some(i);
        Ok(self.heads[i].take())
    }
    pub fn next_event(&mut self) -> Result<Option<MarketEvent>, MergeError> {
        if self.failed {
            return Err(MergeError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "merged replay poisoned",
            )));
        }
        let result = self.next_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn validate<const N: usize, const G: usize>(
        &self,
        engine: &Consolidator<V, N, G>,
    ) -> Result<(), MergeError> {
        for r in &self.readers {
            let m = r.metadata;
            let config = engine.venue_config(m.venue)?;
            let native = config.metadata;
            if m.instrument != native.instrument
                || m.price_decimals != native.price.decimals
                || m.tick_atoms != native.price.atoms
                || m.quantity_decimals != native.quantity.decimals
                || m.quantity_atoms != native.quantity.atoms
            {
                return Err(MergeError::MetadataMismatch);
            }
        }
        Ok(())
    }
    /// Maximum-throughput merge. Call next_event + engine.apply for caller-controlled stepping.
    pub fn run<const N: usize, const G: usize>(
        &mut self,
        engine: &mut Consolidator<V, N, G>,
    ) -> Result<u64, MergeError> {
        self.validate(engine)?;
        let mut count = 0;
        while let Some(e) = self.next_event()? {
            engine.apply(&e)?;
            count += 1;
        }
        for m in self.metadata() {
            let state = engine.venue_book(VenueId(m.venue.0))?.state();
            if matches!(
                state,
                book::BookState::AwaitingSnapshot | book::BookState::BuildingSnapshot
            ) {
                return Err(MergeError::IncompleteSnapshot);
            }
        }
        engine.levels(Side::Buy)?;
        Ok(count)
    }
}
