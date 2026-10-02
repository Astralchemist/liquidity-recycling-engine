//! Instrumentation stage vocabulary; production histograms are a later milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencyStage {
    Decode,
    Book,
    Statistics,
    Decision,
    OrderConstruction,
    Gateway,
    Ack,
}
