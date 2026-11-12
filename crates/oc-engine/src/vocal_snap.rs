//! Automatic target proposals for `@vocal_tune`.
//!
//! Pure decision code: given a detected pitch, a scale and the neighbouring
//! choices it returns the pitch the note should land on. Audio, analysis and
//! the bake live in [`crate::vocal_tune`]; see `docs/vocal-tune.md` for the
//! tie-break order and the caps.
//!
//! The policy is deliberately small and order-dependent: one forward pass over
//! the detected notes, each choice sees the previous choices and the next
//! note's provisional target. That keeps a proposal reproducible from the
//! analysis alone and makes every rule a pure function.

use oc_dsl::ast::{Accidental, KeyMode, KeySignature, NoteName, VocalSnapPolicy, VocalTune};
use oc_dsp::vocal_analysis::VocalNote;

/// A note this close to a scale pitch is already in tune: no correction.
pub const TOLERANCE_CENTS: f64 = 10.0;
/// Inside this window the contour tie-break may override the nearer pitch.
pub const WINDOW_CENTS: f64 = 35.0;
/// Default largest scale-snap correction. A wider move is not proposed.
pub const CAP_CENTS: f64 = 100.0;

/// The three knobs of one automatic pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapConfig {
    pub tolerance_cents: f64,
    pub window_cents: f64,
    pub cap_cents: f64,
}

impl Default for SnapConfig {
    fn default() -> Self {
        Self {
            tolerance_cents: TOLERANCE_CENTS,
            window_cents: WINDOW_CENTS,
            cap_cents: CAP_CENTS,
        }
    }
}

impl SnapConfig {
    /// Snap settings derived from a tune.
    pub fn from_tune(tune: &VocalTune) -> Self {
        Self {
            window_cents: tune.snap_window_cents,
            cap_cents: tune.snap_cap_cents,
            ..Self::default()
        }
    }
}

/// What the scale chooser decided for one note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapOutcome {
    /// Already within [`TOLERANCE_CENTS`]: the note is not touched.
    InTune,
    /// A target within the cap was chosen.
    Snapped,
    /// No scale pitch is within the cap: the note is left alone.
    Capped,
}

/// One automatic proposal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapProposal {
    /// Absolute target in MIDI cents; `None` when nothing may move.
    pub target_cents: Option<f64>,
    pub outcome: SnapOutcome,
}

impl SnapProposal {
    fn capped() -> Self {
        Self {
            target_cents: None,
            outcome: SnapOutcome::Capped,
        }
    }
}

/// Pitch class of a note name, 0 = C.
pub fn pitch_class(note: NoteName, accidental: Accidental) -> i32 {
    let base: i32 = match note {
        NoteName::C => 0,
        NoteName::D => 2,
        NoteName::E => 4,
        NoteName::F => 5,
        NoteName::G => 7,
        NoteName::A => 9,
        NoteName::B => 11,
    };
    let shift = match accidental {
        Accidental::Natural => 0,
        Accidental::Sharp => 1,
        Accidental::DoubleSharp => 2,
        Accidental::Flat => -1,
        Accidental::DoubleFlat => -2,
    };
    (base + shift).rem_euclid(12)
}

/// Diatonic steps above the tonic. Minor means natural minor.
pub fn scale_steps(mode: KeyMode) -> [i32; 7] {
    match mode {
        KeyMode::Major => [0, 2, 4, 5, 7, 9, 11],
        KeyMode::Minor => [0, 2, 3, 5, 7, 8, 10],
    }
}

/// The key's pitch classes as flags indexed by MIDI pitch class.
pub fn scale_pitch_classes(scale: &KeySignature) -> [bool; 12] {
    let tonic = pitch_class(scale.root, scale.accidental);
    let mut mask = [false; 12];
    for step in scale_steps(scale.mode) {
        mask[((tonic + step).rem_euclid(12)) as usize] = true;
    }
    mask
}

/// Every scale pitch inside the MIDI range, ascending.
fn scale_grid(mask: &[bool; 12]) -> Vec<f64> {
    (0..=127)
        .filter(|midi| mask[midi % 12])
        .map(|midi| midi as f64 * 100.0)
        .collect()
}

/// Closest scale pitch to `detected_cents`, ignoring the cap.
pub fn nearest_scale_cents(detected_cents: f64, mask: &[bool; 12]) -> Option<f64> {
    scale_grid(mask).into_iter().min_by(|left, right| {
        (left - detected_cents)
            .abs()
            .total_cmp(&(right - detected_cents).abs())
            .then(left.total_cmp(right))
    })
}

/// Scale pitches within `cap_cents` of `detected_cents`, ascending.
pub fn candidates(detected_cents: f64, mask: &[bool; 12], cap_cents: f64) -> Vec<f64> {
    scale_grid(mask)
        .into_iter()
        .filter(|cents| (cents - detected_cents).abs() <= cap_cents)
        .collect()
}

fn step(from: f64, to: f64) -> i8 {
    let delta = to - from;
    if delta.abs() < 1e-9 {
        0
    } else if delta > 0.0 {
        1
    } else {
        -1
    }
}

/// Chosen intervals that disagree with the performed contour.
///
/// Zero means the choice moves the way the singer moved; one or two means it
/// flattens or reverses a move that the take actually made.
fn reversals(
    detected_cents: f64,
    candidate: f64,
    prev_detected: Option<f64>,
    prev_chosen: Option<f64>,
    next_detected: Option<f64>,
    next_chosen: Option<f64>,
) -> u8 {
    let mut count = 0;
    if let (Some(performed_from), Some(chosen_from)) = (prev_detected, prev_chosen) {
        let performed = step(performed_from, detected_cents);
        if performed != 0 && step(chosen_from, candidate) != performed {
            count += 1;
        }
    }
    if let (Some(performed_to), Some(chosen_to)) = (next_detected, next_chosen) {
        let performed = step(detected_cents, performed_to);
        if performed != 0 && step(candidate, chosen_to) != performed {
            count += 1;
        }
    }
    count
}

/// Choose a target for one note, in the documented tie-break order.
///
/// `prev_detected`/`prev_chosen` are the previous participating note's pitch
/// and its chosen target; `next_detected`/`next_chosen` the next note's pitch
/// and its provisional target. `None` where a note does not participate in the
/// contour (a bypassed note, a rest, no neighbour).
pub fn snap_note(
    detected_cents: f64,
    mask: &[bool; 12],
    config: SnapConfig,
    prev_detected: Option<f64>,
    prev_chosen: Option<f64>,
    next_detected: Option<f64>,
    next_chosen: Option<f64>,
) -> SnapProposal {
    let Some(nearest) = nearest_scale_cents(detected_cents, mask) else {
        return SnapProposal::capped();
    };
    if (nearest - detected_cents).abs() <= config.tolerance_cents {
        return SnapProposal {
            target_cents: Some(nearest),
            outcome: SnapOutcome::InTune,
        };
    }
    let within = candidates(detected_cents, mask, config.cap_cents);
    if within.is_empty() {
        return SnapProposal::capped();
    }
    // The two nearest candidates are the ones either side of the note; the
    // choice is ambiguous only when the nearer one wins by at most `window`.
    let mut by_distance = within.clone();
    by_distance.sort_by(|left, right| {
        (left - detected_cents)
            .abs()
            .total_cmp(&(right - detected_cents).abs())
            .then(left.total_cmp(right))
    });
    let mut pick = by_distance[0];
    if by_distance.len() > 1 {
        let near = by_distance[0];
        let far = by_distance[1];
        let gap = (far - detected_cents).abs() - (near - detected_cents).abs();
        if gap <= config.window_cents {
            let options = [near, far];
            pick = options
                .into_iter()
                .min_by(|left, right| {
                    let key = |value: f64| {
                        (
                            reversals(
                                detected_cents,
                                value,
                                prev_detected,
                                prev_chosen,
                                next_detected,
                                next_chosen,
                            ),
                            (value - detected_cents).abs(),
                            value,
                        )
                    };
                    let (left_reversals, left_distance, left_value) = key(*left);
                    let (right_reversals, right_distance, right_value) = key(*right);
                    left_reversals
                        .cmp(&right_reversals)
                        .then(left_distance.total_cmp(&right_distance))
                        .then(left_value.total_cmp(&right_value))
                })
                .unwrap_or(near);
        }
    }
    SnapProposal {
        target_cents: Some(pick),
        outcome: SnapOutcome::Snapped,
    }
}

/// Fold `target_cents` by whole octaves so it lands within ±600 cents of
/// `near_cents`. A reference take that doubles an octave higher or lower
/// therefore keeps the take in its own register.
pub fn fold_octaves(target_cents: f64, near_cents: f64) -> f64 {
    let octaves = ((near_cents - target_cents) / 1_200.0).round();
    target_cents + 1_200.0 * octaves
}

/// The reference note that corresponds to a take note starting at `local_s`
/// seconds inside the reference clip.
///
/// A note counts when its span covers the time, or when its onset is within
/// `tolerance_s`. Anything else is a gap in the reference: the caller falls
/// back to the scale rather than stretching a note across the gap.
pub fn reference_note(notes: &[VocalNote], local_s: f64, tolerance_s: f64) -> Option<&VocalNote> {
    if let Some(note) = notes
        .iter()
        .find(|note| local_s >= note.start_s && local_s < note.end_s)
    {
        return Some(note);
    }
    notes
        .iter()
        .min_by(|left, right| {
            (left.start_s - local_s)
                .abs()
                .total_cmp(&(right.start_s - local_s).abs())
        })
        .filter(|note| (note.start_s - local_s).abs() <= tolerance_s)
}

/// The policy a block actually runs; `auto` picks by `reference`.
pub fn resolved_policy(tune: &VocalTune) -> VocalSnapPolicy {
    tune.snap.resolved(tune.reference.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(root: NoteName, accidental: Accidental, mode: KeyMode) -> KeySignature {
        KeySignature {
            root,
            accidental,
            mode,
        }
    }

    fn c_major() -> KeySignature {
        key(NoteName::C, Accidental::Natural, KeyMode::Major)
    }

    fn config(cap: f64) -> SnapConfig {
        SnapConfig {
            cap_cents: cap,
            ..SnapConfig::default()
        }
    }

    fn note(start_s: f64, end_s: f64, f0_median_hz: f64, confidence: f64) -> VocalNote {
        VocalNote {
            start_s,
            end_s,
            onset_s: start_s,
            offset_s: end_s,
            f0_median_hz,
            confidence,
            voiced_ratio: 1.0,
        }
    }

    #[test]
    fn scale_pitch_classes_follow_the_mode() {
        let mask = scale_pitch_classes(&c_major());
        for pc in [0, 2, 4, 5, 7, 9, 11] {
            assert!(mask[pc], "C major should contain {pc}");
        }
        for pc in [1, 3, 6, 8, 10] {
            assert!(!mask[pc], "C major should not contain {pc}");
        }
        let a_minor = scale_pitch_classes(&key(NoteName::A, Accidental::Natural, KeyMode::Minor));
        for pc in [9, 11, 0, 2, 4, 5, 7] {
            assert!(a_minor[pc], "A minor should contain {pc}");
        }
    }

    #[test]
    fn candidates_are_ordered_and_inside_the_cap() {
        let mask = scale_pitch_classes(&c_major());
        // C4 = 6000 cents; a 35-cent cap leaves only C4.
        assert_eq!(candidates(6_020.0, &mask, 35.0), vec![6_000.0]);
        // A 120-cent cap leaves the two neighbours only.
        assert_eq!(candidates(6_100.0, &mask, 120.0), vec![6_000.0, 6_200.0]);
        assert_eq!(nearest_scale_cents(6_100.0, &mask), Some(6_000.0));
    }

    #[test]
    fn an_in_tune_note_keeps_its_pitch() {
        let mask = scale_pitch_classes(&c_major());
        // D4 = 6200 cents.
        let proposal = snap_note(
            6_204.0,
            &mask,
            SnapConfig::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(proposal.outcome, SnapOutcome::InTune);
        assert_eq!(proposal.target_cents, Some(6_200.0));
        // 10 cents is the boundary: at 9.9 the note is untouched, at 10.1 it
        // is a proposal.
        let inside = snap_note(
            6_209.9,
            &mask,
            SnapConfig::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(inside.outcome, SnapOutcome::InTune);
        let outside = snap_note(
            6_210.1,
            &mask,
            SnapConfig::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(outside.outcome, SnapOutcome::Snapped);
        assert_eq!(outside.target_cents, Some(6_200.0));
    }

    #[test]
    fn outside_the_window_the_nearest_pitch_wins() {
        let mask = scale_pitch_classes(&c_major());
        // 30 cents above D4: the neighbour (E4) is 170 cents further away.
        let proposal = snap_note(
            6_230.0,
            &mask,
            SnapConfig::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(proposal.target_cents, Some(6_200.0));
        // 40 cents above D4 with a 35-cent window: still the nearer pitch.
        let proposal = snap_note(
            6_240.0,
            &mask,
            SnapConfig::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(proposal.target_cents, Some(6_200.0));
    }

    #[test]
    fn inside_the_window_the_performed_direction_decides() {
        let mask = scale_pitch_classes(&c_major());
        // Between E4 (6400) and F4 (6500): 6460 is 60 from E, 40 from F, so
        // the window decides. The take is falling from 6520 to 6460, and E4
        // continues that fall while F4 would flatten it.
        let proposal = snap_note(
            6_460.0,
            &mask,
            SnapConfig::default(),
            Some(6_520.0),
            Some(6_500.0),
            None,
            None,
        );
        assert_eq!(proposal.target_cents, Some(6_400.0));
        // Without any contour the nearer pitch wins.
        let proposal = snap_note(
            6_460.0,
            &mask,
            SnapConfig::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(proposal.target_cents, Some(6_500.0));
    }

    #[test]
    fn a_performed_peak_snaps_to_the_peak_candidate() {
        let mask = scale_pitch_classes(&c_major());
        // The take rises from 6400 to 6460 and falls back to 6400, so the
        // ambiguous note is a peak: F4 matches both performed intervals.
        let proposal = snap_note(
            6_460.0,
            &mask,
            SnapConfig::default(),
            Some(6_400.0),
            Some(6_400.0),
            Some(6_400.0),
            Some(6_400.0),
        );
        assert_eq!(proposal.target_cents, Some(6_500.0));
        // The same pitch in a monotone line is not a peak, so the nearer
        // pitch wins.
        let flat = snap_note(
            6_460.0,
            &mask,
            SnapConfig::default(),
            Some(6_450.0),
            Some(6_500.0),
            Some(6_450.0),
            Some(6_500.0),
        );
        assert_eq!(flat.target_cents, Some(6_500.0));
    }

    #[test]
    fn a_note_beyond_the_cap_is_capped() {
        let mask = scale_pitch_classes(&c_major());
        // 90 cents above C4: the nearest scale pitch is 90 away.
        let capped = snap_note(6_090.0, &mask, config(50.0), None, None, None, None);
        assert_eq!(capped.outcome, SnapOutcome::Capped);
        assert_eq!(capped.target_cents, None);
        // The same note moves when the user raises the cap.
        let moved = snap_note(6_090.0, &mask, config(100.0), None, None, None, None);
        assert_eq!(moved.outcome, SnapOutcome::Snapped);
        assert_eq!(moved.target_cents, Some(6_000.0));
    }

    #[test]
    fn a_note_within_tolerance_is_in_tune_even_under_a_tight_cap() {
        let mask = scale_pitch_classes(&c_major());
        let proposal = snap_note(6_004.0, &mask, config(0.0), None, None, None, None);
        assert_eq!(proposal.outcome, SnapOutcome::InTune);
        assert_eq!(proposal.target_cents, Some(6_000.0));
    }

    #[test]
    fn octave_folding_keeps_the_take_in_its_register() {
        // A reference an octave up folds down; an octave down folds up; a
        // note already close is unchanged.
        assert_eq!(fold_octaves(8_100.0, 6_900.0), 6_900.0);
        assert_eq!(fold_octaves(5_700.0, 6_900.0), 6_900.0);
        assert_eq!(fold_octaves(6_930.0, 6_900.0), 6_930.0);
        assert_eq!(fold_octaves(8_130.0, 6_900.0), 6_930.0);
    }

    #[test]
    fn reference_notes_match_covering_spans_and_near_onsets() {
        let notes = vec![note(0.2, 0.7, 220.0, 0.9), note(1.0, 1.5, 330.0, 0.9)];
        assert_eq!(
            reference_note(&notes, 0.4, 0.03).map(|n| n.start_s),
            Some(0.2)
        );
        // A take note 20 ms before the reference onset still matches it.
        assert_eq!(
            reference_note(&notes, 0.98, 0.03).map(|n| n.start_s),
            Some(1.0)
        );
        // A rest between the two notes is not stretched.
        assert_eq!(reference_note(&notes, 0.85, 0.03), None);
        // A low-confidence reference note is the caller's business, not ours.
        assert_eq!(reference_note(&[], 0.4, 0.03), None);
    }
}
