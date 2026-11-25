//! Controls owned by the mixer insert instead of the device it hosts.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotControl {
    Wet,
    Enabled,
}

impl SlotControl {
    pub const WET_ID: &'static str = "slot/wet";
    pub const ENABLED_ID: &'static str = "slot/enabled";
    pub const DEFAULT_WET: f64 = 1.0;
    pub const WET_MIN: f64 = 0.0;
    pub const WET_MAX: f64 = 1.0;

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            Self::WET_ID => Some(Self::Wet),
            Self::ENABLED_ID => Some(Self::Enabled),
            _ => None,
        }
    }
}
