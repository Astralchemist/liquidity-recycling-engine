//! Replay calls exactly the VenueBook::apply used by a future live ingress loop.
use book::{BookError, BookState, VenueBook};
use market_events::MarketEvent;
use recorder::RecordingReader;
use std::{
    io::{self, Read},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayMode {
    Maximum,
    Paced { speed: u32 },
}
#[derive(Debug)]
pub enum ReplayError {
    Io(io::Error),
    Book(BookError),
    InvalidSpeed,
    IncompleteBookState,
}
impl From<io::Error> for ReplayError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
pub struct Replay<R> {
    reader: RecordingReader<R>,
    mode: ReplayMode,
    origin: Option<(u64, Instant)>,
}
impl<R: Read> Replay<R> {
    pub fn new(reader: R, mode: ReplayMode) -> Result<Self, ReplayError> {
        if matches!(mode, ReplayMode::Paced { speed: 0 }) {
            return Err(ReplayError::InvalidSpeed);
        }
        Ok(Self {
            reader: RecordingReader::new(reader)?,
            mode,
            origin: None,
        })
    }
    pub fn metadata(&self) -> recorder::RecordingMetadata {
        self.reader.metadata
    }
    /// One call is one step. Pacing changes wall time only, never event timestamps.
    pub fn step<const N: usize>(
        &mut self,
        book: &mut VenueBook<N>,
    ) -> Result<Option<MarketEvent>, ReplayError> {
        let Some(event) = self.reader.next_event()? else {
            return if book.state() == BookState::Live {
                Ok(None)
            } else {
                Err(ReplayError::IncompleteBookState)
            };
        };
        // Validation (including clock checks) always runs in the shared book module.
        // No caller can observe this step until its pacing delay completes.
        book.apply(&event).map_err(ReplayError::Book)?;
        if let ReplayMode::Paced { speed } = self.mode {
            let (first, start) = *self
                .origin
                .get_or_insert((event.receive_ts.0, Instant::now()));
            let due = Duration::from_nanos((event.receive_ts.0 - first) / u64::from(speed));
            if let Some(wait) = due.checked_sub(start.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        Ok(Some(event))
    }
    pub fn run<const N: usize>(&mut self, book: &mut VenueBook<N>) -> Result<u64, ReplayError> {
        let mut count = 0;
        while self.step(book)?.is_some() {
            count += 1;
        }
        Ok(count)
    }
}

pub mod merged;
