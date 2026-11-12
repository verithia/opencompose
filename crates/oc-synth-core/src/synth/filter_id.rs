//! Filter-class mapping for imported filter IDs.

use super::{FilterMode, FilterModel};

/// Map an imported filter ID to the engine's coarse response family.
///
/// The response is encoded in the ID prefix: `L*`/`LBH*` are low-pass,
/// `H*` high-pass, `B*`/`BP*` band-pass, and `N*` notch (for example `L12`,
/// `H24`, `B12`, `N12`, `LBH12`). Long descriptive IDs without a matching
/// prefix keep the low-pass fallback.
pub fn mode(id: &str) -> FilterMode {
    let upper = id.to_ascii_uppercase();
    if upper == "BYPASS"
        || upper.contains("COMB")
        || upper.contains("PHASE")
        || upper.contains("FORMANT")
    {
        FilterMode::LowPass
    } else if upper.contains("NOTCH") || upper == "BANDREJECT" {
        FilterMode::Notch
    } else if upper.starts_with("LEQ") {
        // `LEQ6`/`LEQ12` are low-shelf cuts: measured as a ~6 dB/oct rise
        // towards the highs, so a high-pass is the closest response.
        FilterMode::HighPass
    } else if upper.contains("EQ") {
        // `HEQ12`/`HEQ6`/`BEQ12` are EQ bands, not highpass/bandpass. The
        // engine has no shelf/peak response, so keep the low-pass fallback
        // rather than removing the whole low end (the FXFilter alone turned PL
        // into silence).
        FilterMode::LowPass
    } else if upper.starts_with('B') {
        FilterMode::BandPass
    } else if upper.starts_with('H') {
        FilterMode::HighPass
    } else if upper.starts_with('N') {
        FilterMode::Notch
    } else {
        FilterMode::LowPass
    }
}

/// Every id in the state-variable family: the plain slopes, the 13 two-way
/// morphs and the 8 three-way morphs. The suffix picks the slope, not a class.
const SVF_FILTER_IDS: [&str; 35] = [
    "L6", "L12", "L18", "L24", "H6", "H12", "H18", "H24", "B12", "B24", "P12", "P24", "N12", "N24",
    "LH6", "LH12", "LB12", "LP12", "LN12", "HB12", "HP12", "HN12", "BP12", "BN12", "PP12", "PN12",
    "NN12", "LBH12", "LBH24", "LPH12", "LPH24", "LNH12", "LNH24", "BPN12", "BPN24",
];

/// The Moog class: four taps of one four-pole model, in slope order.
const MOOG_FILTER_IDS: [(&str, FilterModel); 4] = [
    ("MGL6", FilterModel::Moog6),
    ("MGL12", FilterModel::Moog12),
    ("MGL18", FilterModel::Moog18),
    ("MGL24", FilterModel::Moog24),
];

/// The three one-off ladder ids; `DirtyMg` is the same loop with saturating
/// stages. No other filter id uses a ladder topology.
const LADDER_FILTER_IDS: [&str; 3] = ["LADDERMG", "LADDERACID", "LADDEREMS"];

/// Map a filter id to its engine class and slope, by explicit class table.
pub fn model(id: &str) -> FilterModel {
    let upper = id.to_ascii_uppercase();
    if let Some((_, model)) = MOOG_FILTER_IDS.iter().find(|(name, _)| *name == upper) {
        return *model;
    }
    if LADDER_FILTER_IDS.contains(&upper.as_str()) {
        return FilterModel::Ladder;
    }
    if upper == "DIRTYMG" {
        return FilterModel::DirtyLadder;
    }
    if SVF_FILTER_IDS.contains(&upper.as_str()) {
        return svf_slope_model(&upper);
    }
    // Classes the engine does not model run the state-variable response.
    svf_slope_model(&upper)
}

/// Slope variant of the state-variable response; also the fallback for the
/// classes the engine approximates with an SVF (EQ shelves, comb and phase).
fn svf_slope_model(id: &str) -> FilterModel {
    if id.ends_with('6') && matches!(mode(id), FilterMode::LowPass | FilterMode::HighPass) {
        FilterModel::Svf1
    } else if id.ends_with("18") {
        FilterModel::Svf3
    } else if id.ends_with("24") {
        FilterModel::Svf4
    } else {
        FilterModel::Svf
    }
}
