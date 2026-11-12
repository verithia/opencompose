//! Close ownership for gestures sharing one undo entry. Tokens do not split
//! concurrent edits into separate entries; they prevent one owner's late close
//! from consuming another owner's membership or a legacy nesting level.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);

/// Opaque within the running backend's IPC lifetime. Decimal identities are
/// strings on the wire, so JavaScript cannot round their u64 values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UndoGestureToken(String);

/// Opaque capture of this ownership authority's current target topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParameterTargetStamp(String);

#[derive(Debug, PartialEq, Eq)]
/// Why a parameter target stamp could not be used.
pub enum ParameterTargetError {
    IdentityExhausted,
    Changed,
}

impl std::fmt::Display for ParameterTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityExhausted => {
                std::fmt::Display::fmt(&UndoGestureError::IdentityExhausted, formatter)
            }
            Self::Changed => {
                formatter.write_str("Parameter target changed before gesture admission")
            }
        }
    }
}
impl std::error::Error for ParameterTargetError {}

#[derive(Debug)]
/// Why an undo gesture could not start.
pub enum UndoGestureError {
    IdentityExhausted,
}

impl std::fmt::Display for UndoGestureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityExhausted => {
                formatter.write_str("Undo gesture identity space is exhausted")
            }
        }
    }
}

impl std::error::Error for UndoGestureError {}

fn next_identity(counter: &AtomicU64) -> Result<u64, UndoGestureError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| UndoGestureError::IdentityExhausted)
}

pub(super) struct GestureOwnership {
    admission: Option<ParameterTargetStamp>,
    stack: Option<u64>,
    document: Option<u64>,
    live: HashSet<UndoGestureToken>,
}
impl Default for GestureOwnership {
    fn default() -> Self {
        Self::with_identity_counter(&NEXT_IDENTITY)
    }
}
impl GestureOwnership {
    fn with_identity_counter(counter: &AtomicU64) -> Self {
        Self {
            admission: next_identity(counter)
                .ok()
                .map(|identity| ParameterTargetStamp(identity.to_string())),
            stack: None,
            document: None,
            live: HashSet::new(),
        }
    }

    pub(super) fn parameter_target_stamp(&self) -> Option<&ParameterTargetStamp> {
        self.admission.as_ref()
    }

    pub(super) fn require_parameter_target_stamp(
        &self,
        expected: &ParameterTargetStamp,
    ) -> Result<(), ParameterTargetError> {
        match self.admission.as_ref() {
            None => Err(ParameterTargetError::IdentityExhausted),
            Some(current) if current == expected => Ok(()),
            Some(_) => Err(ParameterTargetError::Changed),
        }
    }

    pub(super) fn begin(&mut self) -> Result<UndoGestureToken, UndoGestureError> {
        let stack = match self.stack {
            Some(identity) => identity,
            None => next_identity(&NEXT_IDENTITY)?,
        };
        let document = match self.document {
            Some(identity) => identity,
            None => next_identity(&NEXT_IDENTITY)?,
        };
        let gesture = next_identity(&NEXT_IDENTITY)?;
        let token = UndoGestureToken(format!("{stack}:{document}:{gesture}"));
        self.stack = Some(stack);
        self.document = Some(document);
        self.live.insert(token.clone());
        Ok(token)
    }

    pub(super) fn close(&mut self, token: &UndoGestureToken) -> bool {
        self.live.remove(token)
    }
    pub(super) fn contains(&self, token: &UndoGestureToken) -> bool {
        self.live.contains(token)
    }

    pub(super) fn has_owned(&self) -> bool {
        !self.live.is_empty()
    }
    pub(super) fn invalidate(&mut self) {
        self.invalidate_with_identity_counter(&NEXT_IDENTITY);
    }

    fn invalidate_with_identity_counter(&mut self, counter: &AtomicU64) {
        self.live.clear();
        // Capture allocation happens on invalidation, never during snapshot reads.
        // Exhaustion must retire the old capture instead of keeping it admissible.
        self.admission = next_identity(counter)
            .ok()
            .map(|identity| ParameterTargetStamp(identity.to_string()));
    }
    pub(super) fn replace_document(&mut self) {
        self.invalidate();
        self.document = None;
    }
}

/// Named presets remain the same logical target across patch replacement,
/// inheritance detachment and parameter edits. Track bindings are index-based,
/// so changing their order, names or instrument/preset assignment expires owners.
/// Chain slots and bus/master targets also expire owners on structural changes.
/// This conservatively includes a same-named patch replacement that changes FX.
pub(super) fn target_topology_changed(
    old: &oc_dsl::ast::Project,
    new: &oc_dsl::ast::Project,
) -> bool {
    !old.presets
        .iter()
        .map(|preset| &preset.name)
        .eq(new.presets.iter().map(|preset| &preset.name))
        || !old
            .tracks
            .iter()
            .map(|track| (&track.name, &track.instrument, &track.preset))
            .eq(new
                .tracks
                .iter()
                .map(|track| (&track.name, &track.instrument, &track.preset)))
        || !old
            .buses
            .iter()
            .map(|bus| &bus.name)
            .eq(new.buses.iter().map(|bus| &bus.name))
        || old.master.is_some() != new.master.is_some()
        || !effect_chains(old)
            .zip(effect_chains(new))
            .all(|(left, right)| {
                left.iter()
                    .map(|effect| &effect.effect_type)
                    .eq(right.iter().map(|effect| &effect.effect_type))
            })
}

fn effect_chains(
    project: &oc_dsl::ast::Project,
) -> impl Iterator<Item = &Vec<oc_dsl::ast::Effect>> {
    project
        .tracks
        .iter()
        .map(|owner| &owner.effects)
        .chain(project.presets.iter().map(|owner| &owner.effects))
        .chain(project.buses.iter().map(|owner| &owner.effects))
        .chain(project.master.iter().map(|owner| &owner.effects))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_admission_capture_is_unique_serialized_and_independent_of_close() {
        let mut first = GestureOwnership::default();
        let second = GestureOwnership::default();
        let original = first.parameter_target_stamp().unwrap().clone();
        assert_ne!(Some(&original), second.parameter_target_stamp());
        let member = first.begin().unwrap();
        assert_eq!(first.parameter_target_stamp(), Some(&original));
        assert!(first.close(&member));
        assert_eq!(first.parameter_target_stamp(), Some(&original));
        let encoded = serde_json::to_value(&original).unwrap();
        assert!(encoded.is_string());
        assert_eq!(
            serde_json::from_value::<ParameterTargetStamp>(encoded).unwrap(),
            original
        );
        first.invalidate();
        let changed = first.parameter_target_stamp().unwrap().clone();
        assert_ne!(changed, original);
        assert_eq!(
            first.require_parameter_target_stamp(&original),
            Err(ParameterTargetError::Changed)
        );
        first.replace_document();
        assert_ne!(first.parameter_target_stamp(), Some(&changed));
    }

    #[test]
    fn parameter_admission_identity_exhaustion_retires_old_capture_without_fallback() {
        let exhausted = AtomicU64::new(u64::MAX);
        let unavailable = GestureOwnership::with_identity_counter(&exhausted);
        assert!(unavailable.parameter_target_stamp().is_none());
        let counter = AtomicU64::new(u64::MAX - 1);
        let mut owner = GestureOwnership::with_identity_counter(&counter);
        let original = owner.parameter_target_stamp().unwrap().clone();
        assert!(owner.require_parameter_target_stamp(&original).is_ok());
        let member = owner.begin().unwrap();
        owner.invalidate_with_identity_counter(&counter);
        assert!(owner.parameter_target_stamp().is_none());
        assert_eq!(
            owner.require_parameter_target_stamp(&original),
            Err(ParameterTargetError::IdentityExhausted)
        );
        assert_eq!(
            unavailable.require_parameter_target_stamp(&original),
            Err(ParameterTargetError::IdentityExhausted)
        );
        assert!(!owner.contains(&member));
        assert!(!owner.has_owned());
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn parameter_admission_exhausted_capture_refuses_command_before_membership_mint() {
        use crate::state::{AppState, UndoScope};
        use tauri::{
            Manager,
            test::{mock_builder, mock_context, noop_assets},
        };
        let state = AppState::new();
        let expected = state
            .lock_inner()
            .undo
            .parameter_target_stamp()
            .unwrap()
            .clone();
        let exhausted = AtomicU64::new(u64::MAX);
        state
            .lock_inner()
            .undo
            .gesture_ownership
            .invalidate_with_identity_counter(&exhausted);
        let app = mock_builder()
            .manage(state)
            .build(mock_context(noop_assets()))
            .unwrap();
        assert_eq!(
            crate::commands::begin_parameter_gesture(UndoScope::Preset, expected, app.state())
                .unwrap_err(),
            UndoGestureError::IdentityExhausted.to_string()
        );
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        assert!(inner.undo.parameter_target_stamp().is_none());
        assert!(!inner.undo.is_in_gesture());
        assert!(inner.undo.undo.is_empty());
        assert!(inner.undo.redo.is_empty());
        assert!(!inner.undo.gesture_ownership.has_owned());
    }

    #[test]
    fn owned_undo_identity_never_wraps_or_restarts_with_a_stack_or_document() {
        let exhausted = AtomicU64::new(u64::MAX);
        assert!(matches!(
            next_identity(&exhausted),
            Err(UndoGestureError::IdentityExhausted)
        ));
        assert_eq!(exhausted.load(Ordering::Relaxed), u64::MAX);
        let mut first = GestureOwnership::default();
        let old = first.begin().unwrap();
        let old_ids: Vec<_> = old.0.split(':').collect();
        first.replace_document();
        let current = first.begin().unwrap();
        let current_ids: Vec<_> = current.0.split(':').collect();
        assert_eq!(old_ids[0], current_ids[0]);
        assert_ne!(old_ids[1], current_ids[1]);
        assert_ne!(old_ids[2], current_ids[2]);
        assert!(!first.close(&old));
        assert!(first.has_owned());
        let mut second = GestureOwnership::default();
        let other = second.begin().unwrap();
        assert_ne!(current_ids[0], other.0.split(':').next().unwrap());
        assert!(!second.close(&current));
        assert!(second.has_owned());
        let wire = serde_json::to_value(&current).unwrap();
        assert!(wire.is_string());
        assert_eq!(
            serde_json::from_value::<UndoGestureToken>(wire).unwrap(),
            current
        );
    }

    #[test]
    fn owned_undo_nested_closes_do_not_consume_other_tokens_or_legacy_depth() {
        use crate::state::{UndoScope, UndoStack};
        let mut stack = UndoStack::new();
        let first = stack.begin_owned_gesture(UndoScope::Project).unwrap();
        let second = stack.begin_owned_gesture(UndoScope::MixerLevels).unwrap();
        stack.end_gesture();
        assert!(stack.is_in_gesture());
        assert!(stack.end_owned_gesture(&first));
        assert!(!stack.end_owned_gesture(&first));
        assert!(stack.is_in_gesture());
        stack.begin_gesture(UndoScope::Preset);
        assert!(stack.end_owned_gesture(&second));
        assert!(stack.is_in_gesture(), "legacy owner still holds one level");
        stack.end_gesture();
        assert!(!stack.is_in_gesture());
        let current = stack.begin_owned_gesture(UndoScope::Project).unwrap();
        assert!(!stack.end_owned_gesture(&second));
        assert!(!stack.end_owned_gesture(&UndoGestureToken("unknown".into())));
        stack.end_gesture();
        assert!(stack.is_in_gesture());
        assert!(stack.end_owned_gesture(&current));
        assert!(!stack.is_in_gesture());
    }

    #[test]
    fn owned_undo_tokens_share_the_existing_merge_entry_without_ending_on_push() {
        use crate::state::{UndoEdit, UndoScope, UndoStack, default_project};
        let mut stack = UndoStack::new();
        let mut project = default_project();
        let original = project.clone();
        let first = stack.begin_owned_gesture(UndoScope::Project).unwrap();
        project.properties.tempo += 1.0;
        stack
            .push(
                original.clone(),
                &project,
                UndoEdit::new("first", UndoScope::Project),
            )
            .unwrap();
        let second = stack.begin_owned_gesture(UndoScope::MixerLevels).unwrap();
        assert!(stack.end_owned_gesture(&first));
        let old = project.clone();
        project.properties.tempo += 1.0;
        stack
            .push(
                old,
                &project,
                UndoEdit::new("second", UndoScope::MixerLevels),
            )
            .unwrap();
        assert!(stack.is_in_gesture());
        assert_eq!(
            stack.undo.len(),
            1,
            "tokens isolate close ownership, not separate entries"
        );
        assert!(stack.end_owned_gesture(&second));
        stack.undo(&mut project).unwrap();
        assert_eq!(project, original);
    }

    #[test]
    fn owned_undo_actual_history_resets_invalidate_old_tokens_without_touching_new_owners() {
        use crate::state::{UndoEdit, UndoScope, UndoStack, default_project};
        for operation in ["undo", "redo", "level", "restore", "squash", "clear"] {
            let mut stack = UndoStack::new();
            let mut project = default_project();
            let original = project.clone();
            let mark = stack.mark();
            project.properties.tempo += 1.0;
            stack
                .push(
                    original.clone(),
                    &project,
                    UndoEdit::new("edit", UndoScope::Project),
                )
                .unwrap();
            if operation == "redo" {
                stack.undo(&mut project).unwrap();
            }
            let stale = stack.begin_owned_gesture(UndoScope::Project).unwrap();
            match operation {
                "undo" => {
                    stack.undo(&mut project).unwrap();
                }
                "redo" => {
                    stack.redo(&mut project).unwrap();
                }
                "level" => {
                    stack.restore_undo_level(0, &mut project).unwrap();
                }
                "restore" => stack.restore(mark),
                "squash" => stack.squash_since(
                    mark,
                    original,
                    &project,
                    UndoEdit::new("squash", UndoScope::Project),
                ),
                "clear" => stack.clear(),
                _ => unreachable!(),
            }
            assert!(!stack.is_in_gesture(), "{operation}");
            let current = stack.begin_owned_gesture(UndoScope::Preset).unwrap();
            assert!(!stack.end_owned_gesture(&stale), "{operation}");
            assert!(stack.is_in_gesture(), "{operation}");
            assert!(stack.end_owned_gesture(&current), "{operation}");
        }
    }

    #[test]
    fn owned_undo_failed_history_operation_preserves_the_existing_owner() {
        use crate::state::{UndoScope, UndoStack, default_project};
        let mut stack = UndoStack::new();
        let mut project = default_project();
        let owner = stack.begin_owned_gesture(UndoScope::Project).unwrap();
        assert!(stack.undo(&mut project).is_err());
        assert!(stack.is_in_gesture());
        assert!(stack.end_owned_gesture(&owner));
    }
}
