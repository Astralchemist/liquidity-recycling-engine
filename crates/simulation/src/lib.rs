//! Synthetic markets for controlled engine scenarios. Not a matching or queue simulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    RevisitOscillation,
    OneWayContinuation,
    LocalVoid,
    GlobalVoid,
    ToxicRecovery,
    EmergencyExit,
}
impl Scenario {
    pub const ALL: [Self; 6] = [
        Self::RevisitOscillation,
        Self::OneWayContinuation,
        Self::LocalVoid,
        Self::GlobalVoid,
        Self::ToxicRecovery,
        Self::EmergencyExit,
    ];
    /// Specification §39 letter.
    pub fn letter(self) -> char {
        (b'A' + self as u8) as char
    }
}

pub mod market;
pub mod scenarios;
pub mod structures;
