//! Reserved transparent component observations, no hidden composite score.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ToxicityComponents {
    pub one_sided_aggression_ppm: u32,
    pub depletion_ppm: u32,
    pub cancellation_ppm: u32,
}
