//! Owned automation is recorded before publication, rather than on late close.
use super::{AppStateInner, UndoEdit, UndoGestureToken, undo_gestures};
use oc_dsl::ast::Project;

use super::{GestureState, MAX_UNDO, UndoEntry, UndoStack, diff};
use std::time::SystemTime;

pub(super) struct StagedUndoPush {
    before: Project,
    epoch: u64,
    next_epoch: u64,
    gesture: Option<GestureState>,
    action: StagedAction,
}
enum StagedAction {
    Noop,
    Remove,
    Merge {
        forward: Vec<u8>,
        inverse: Vec<u8>,
        scopes: super::UndoScopes,
    },
    Append(UndoEntry),
}
impl UndoStack {
    pub(super) fn advance_history_epoch(&mut self) {
        self.history_epoch = self.history_epoch.and_then(|epoch| epoch.checked_add(1));
    }
    pub(super) fn stage_push_at(
        &self,
        old: Project,
        new: &Project,
        mut edit: UndoEdit,
        timestamp: SystemTime,
    ) -> Result<StagedUndoPush, String> {
        let epoch = self
            .history_epoch
            .ok_or("Undo history identity space is exhausted")?;
        let next_epoch = epoch
            .checked_add(1)
            .ok_or("Undo history identity space is exhausted")?;
        let gesture_scopes = self.gesture.map(|gesture| gesture.scopes);
        let gesture_started = self.gesture.is_some_and(|gesture| gesture.push_started);
        if let Some(scopes) = gesture_scopes {
            edit.scopes.insert_all(scopes);
        }
        let merge = !self.undo.is_empty()
            && self.merge_base.is_some()
            && if gesture_scopes.is_some() {
                gesture_started
            } else {
                edit.merge
                    && self
                        .undo
                        .last()
                        .is_some_and(|entry| entry.scopes.intersects(edit.scopes))
            };
        let action = if merge {
            if let Some(base) = self.merge_base.as_ref() {
                if let Some(patch) = diff::diff(base, new)
                    .map_err(|error| format!("could not compare the undo history: {error}"))?
                {
                    let inverse = diff::invert_patch(base, new, &patch);
                    StagedAction::Merge {
                        forward: self.encode(&patch)?,
                        inverse: self.encode(&inverse)?,
                        scopes: edit.scopes,
                    }
                } else {
                    StagedAction::Remove
                }
            } else {
                StagedAction::Noop
            }
        } else if let Some(patch) = diff::diff(&old, new)
            .map_err(|error| format!("could not compare the undo history: {error}"))?
        {
            let inverse = diff::invert_patch(&old, new, &patch);
            StagedAction::Append(UndoEntry {
                forward: self.encode(&patch)?,
                inverse: self.encode(&inverse)?,
                label: edit.name,
                scopes: edit.scopes,
                timestamp,
            })
        } else {
            StagedAction::Noop
        };
        Ok(StagedUndoPush {
            before: old,
            epoch,
            next_epoch,
            gesture: self.gesture,
            action,
        })
    }
    pub(super) fn commit_staged_push(&mut self, staged: StagedUndoPush) -> Result<(), String> {
        if self.history_epoch != Some(staged.epoch) || self.gesture != staged.gesture {
            return Err("Undo history changed before the prepared edit was published".into());
        }
        match staged.action {
            StagedAction::Noop => {}
            StagedAction::Remove => {
                self.undo.pop();
                self.merge_base = None;
            }
            StagedAction::Merge {
                forward,
                inverse,
                scopes,
            } => {
                let last = self
                    .undo
                    .last_mut()
                    .ok_or("Undo history changed before the prepared edit was published")?;
                last.forward = forward;
                last.inverse = inverse;
                last.scopes.insert_all(scopes);
            }
            StagedAction::Append(entry) => {
                self.undo.push(entry);
                if self.undo.len() > MAX_UNDO {
                    self.undo.remove(0);
                }
                self.redo.clear();
                self.merge_base = Some(staged.before);
                if let Some(gesture) = &mut self.gesture {
                    gesture.push_started = true;
                }
            }
        }
        self.history_epoch = Some(staged.next_epoch);
        Ok(())
    }
}
impl AppStateInner {
    #[cfg(test)]
    pub(crate) fn set_history_encoding_failure_for_test(&mut self, fail: bool) {
        self.undo.fail_encode = fail;
    }
    /// Record a prepared point before publication using the shared gesture merge
    /// owner. Native changes, when needed, run only after successful encoding.
    pub(crate) fn publish_owned_automation_candidate(
        &mut self,
        token: &UndoGestureToken,
        candidate: Project,
        edit: UndoEdit,
    ) -> Result<(), String> {
        self.publish_owned_automation_with(token, candidate, edit, |_| Ok(()))
    }
    pub(crate) fn publish_owned_automation_with(
        &mut self,
        token: &UndoGestureToken,
        candidate: Project,
        edit: UndoEdit,
        apply: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        self.refuse_in_preview()?;
        if !self.undo.is_owned_gesture_active(token) {
            return Err("Undo gesture is no longer active for this document".into());
        }
        if undo_gestures::target_topology_changed(&self.project, &candidate) {
            return Err("Automation point cannot change its target topology".into());
        }
        let staged =
            self.undo
                .stage_push_at(self.project.clone(), &candidate, edit, SystemTime::now())?;
        let revision = self.revision;
        let content_epoch = self.content_epoch;
        let target_stamp = self.undo.parameter_target_stamp().cloned();
        apply(self)?;
        // An injected/native callback cannot publish over a newer document or
        // history owner. Never restore an old project to conceal that change.
        if self.project != staged.before
            || self.revision != revision
            || self.content_epoch != content_epoch
            || self.undo.parameter_target_stamp() != target_stamp.as_ref()
            || !self.undo.is_owned_gesture_active(token)
        {
            return Err("Automation target changed before the prepared point was published".into());
        }
        self.undo.commit_staged_push(staged)?;
        self.project = candidate;
        self.dsl_source = oc_dsl::serialize(&self.project);
        self.dirty = true;
        self.bump_revision();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, UndoScope};

    #[test]
    fn automation_checkpoint_encoding_failure_preserves_first_and_merged_publications() {
        let state = AppState::new();
        let mut inner = state.lock_inner();
        let token = inner
            .undo
            .begin_owned_gesture(UndoScope::Automation)
            .unwrap();
        for has_entry in [false, true] {
            if has_entry {
                let mut next = inner.project.clone();
                next.name = Some("accepted sibling".into());
                inner
                    .publish_owned_automation_candidate(
                        &token,
                        next,
                        UndoEdit::new("automation", UndoScope::Automation),
                    )
                    .unwrap();
            }
            let project = inner.project.clone();
            let dsl = inner.dsl_source.clone();
            let revision = inner.revision;
            let dirty = inner.dirty;
            let undo: Vec<_> = inner
                .undo
                .undo
                .iter()
                .map(|entry| (entry.forward.clone(), entry.inverse.clone()))
                .collect();
            let redo: Vec<_> = inner
                .undo
                .redo
                .iter()
                .map(|entry| (entry.forward.clone(), entry.inverse.clone()))
                .collect();
            let mut candidate = project.clone();
            candidate.properties.tempo += 1.0;
            inner.undo.fail_encode = true;
            assert!(
                inner
                    .publish_owned_automation_candidate(
                        &token,
                        candidate,
                        UndoEdit::new("automation", UndoScope::Automation)
                    )
                    .is_err()
            );
            inner.undo.fail_encode = false;
            assert_eq!(inner.project, project);
            assert_eq!(inner.dsl_source, dsl);
            assert_eq!(inner.revision, revision);
            assert_eq!(inner.dirty, dirty);
            assert_eq!(
                inner
                    .undo
                    .undo
                    .iter()
                    .map(|entry| (entry.forward.clone(), entry.inverse.clone()))
                    .collect::<Vec<_>>(),
                undo
            );
            assert_eq!(
                inner
                    .undo
                    .redo
                    .iter()
                    .map(|entry| (entry.forward.clone(), entry.inverse.clone()))
                    .collect::<Vec<_>>(),
                redo
            );
            assert!(inner.undo.is_owned_gesture_active(&token));
        }
        inner.undo.end_owned_gesture(&token);
        inner.undo_step().unwrap();
        assert_ne!(inner.project.name.as_deref(), Some("accepted sibling"));
    }
    #[test]
    fn automation_checkpoint_refuses_structural_candidate_without_expiring_owner() {
        let state = AppState::new();
        let mut inner = state.lock_inner();
        let original = inner.project.clone();
        let token = inner
            .undo
            .begin_owned_gesture(UndoScope::Automation)
            .unwrap();
        let mut candidate = original.clone();
        candidate.tracks =
            oc_dsl::parse("@project \"Changed\"\n@track Lead | instrument: synth/lead_saw\n")
                .unwrap()
                .tracks;
        assert!(
            inner
                .publish_owned_automation_candidate(
                    &token,
                    candidate,
                    UndoEdit::new("automation", UndoScope::Automation)
                )
                .is_err()
        );
        assert_eq!(inner.project, original);
        assert!(inner.undo.undo.is_empty());
        assert!(inner.undo.is_owned_gesture_active(&token));
    }
    #[test]
    fn staged_automation_encoding_and_epoch_exhaustion_precede_native_write() {
        let state = AppState::new();
        let mut inner = state.lock_inner();
        let token = inner
            .undo
            .begin_owned_gesture(UndoScope::Automation)
            .unwrap();
        let writes = std::cell::Cell::new(0);
        for failure in ["encode", "exhausted"] {
            let mut candidate = inner.project.clone();
            candidate.properties.tempo += 1.0;
            let before = inner.project.clone();
            let revision = inner.revision;
            if failure == "encode" {
                inner.undo.fail_encode = true;
            } else {
                inner.undo.history_epoch = Some(u64::MAX);
            }
            assert!(
                inner
                    .publish_owned_automation_with(
                        &token,
                        candidate,
                        UndoEdit::new("point", UndoScope::Automation),
                        |_| {
                            writes.set(writes.get() + 1);
                            Ok(())
                        }
                    )
                    .is_err()
            );
            inner.undo.fail_encode = false;
            assert_eq!(writes.get(), 0);
            assert_eq!(inner.project, before);
            assert_eq!(inner.revision, revision);
            assert!(inner.undo.undo.is_empty());
        }
    }
    #[test]
    fn staged_automation_callback_changes_are_refused_without_restoring_stale_document() {
        for change in ["project", "revision", "history"] {
            let state = AppState::new();
            let mut inner = state.lock_inner();
            let token = inner
                .undo
                .begin_owned_gesture(UndoScope::Automation)
                .unwrap();
            let mut candidate = inner.project.clone();
            candidate.properties.tempo += 1.0;
            let old = inner.project.clone();
            assert!(
                inner
                    .publish_owned_automation_with(
                        &token,
                        candidate,
                        UndoEdit::new("point", UndoScope::Automation),
                        |inner| {
                            match change {
                                "project" => {
                                    inner.project.name = Some("new document from callback".into())
                                }
                                "revision" => inner.bump_revision(),
                                _ => inner.undo.clear(),
                            }
                            Ok(())
                        }
                    )
                    .is_err()
            );
            if change == "project" {
                assert_eq!(
                    inner.project.name.as_deref(),
                    Some("new document from callback")
                );
            } else {
                assert_eq!(inner.project, old);
            }
            assert!(inner.undo.undo.is_empty());
        }
    }
    #[test]
    fn staged_history_identity_never_survives_actual_history_or_ownership_reset() {
        for operation in [
            "undo",
            "redo",
            "level",
            "restore",
            "squash",
            "clear",
            "invalidate",
        ] {
            let mut stack = UndoStack::new();
            let mut project = super::super::default_project();
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
            let token = stack.begin_owned_gesture(UndoScope::Automation).unwrap();
            let mut candidate = project.clone();
            candidate.properties.tempo += 1.0;
            let staged = stack
                .stage_push_at(
                    project.clone(),
                    &candidate,
                    UndoEdit::new("point", UndoScope::Automation),
                    SystemTime::now(),
                )
                .unwrap();
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
                    UndoEdit::new("batch", UndoScope::Project),
                ),
                "clear" => stack.clear(),
                _ => stack.invalidate_owned_gestures(),
            }
            let count = stack.undo.len();
            assert!(stack.commit_staged_push(staged).is_err(), "{operation}");
            assert_eq!(stack.undo.len(), count);
            assert!(!stack.is_owned_gesture_active(&token));
        }
    }
}
