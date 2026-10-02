//! Required future scenario catalog. Scenario generation/fill modeling not implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    RevisitOscillation,
    OneWayContinuation,
    LocalVoid,
    GlobalVoid,
    ToxicRecovery,
    EmergencyExit,
}

pub mod structures;
