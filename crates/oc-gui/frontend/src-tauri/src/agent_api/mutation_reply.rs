//! Host-selected replies for mixer mutations; document events remain separate.

use serde::Serialize;
use serde_json::Value;
use tauri::AppHandle;

use crate::agent_tools::tracks::TracksMixArgs;
use crate::commands;
use crate::state::AppStateInner;

use super::{ToolError, state, to_value};

#[derive(Clone, Copy)]
pub(super) enum ReplyProjection {
    Snapshot,
    AgentReceipt,
}

#[derive(Serialize)]
struct AppliedTrackMix {
    index: usize,
    name: String,
    volume_db: f64,
    pan: f64,
}

#[derive(Serialize)]
struct TrackMixReceipt {
    applied: bool,
    revision: u64,
    dirty: bool,
    track_count: usize,
    track: AppliedTrackMix,
}

fn apply_receipt(
    inner: &mut AppStateInner,
    args: &TracksMixArgs,
) -> Result<TrackMixReceipt, String> {
    commands::update_track_mix_inner(inner, args.index, args.volume_db, args.pan)?;
    let track = &inner.project.tracks[args.index];
    Ok(TrackMixReceipt {
        applied: true,
        revision: inner.revision,
        dirty: inner.dirty,
        track_count: inner.project.tracks.len(),
        track: AppliedTrackMix {
            index: args.index,
            name: track.name.clone(),
            volume_db: track.mix.volume_db,
            pan: track.mix.pan,
        },
    })
}

pub(super) fn tracks_mix(
    app: &AppHandle,
    args: TracksMixArgs,
    projection: ReplyProjection,
) -> Result<Value, ToolError> {
    match projection {
        ReplyProjection::Snapshot => to_value(commands::update_track_mix(
            args.index,
            args.volume_db,
            args.pan,
            state(app),
        )?),
        ReplyProjection::AgentReceipt => {
            let state = state(app);
            let mut inner = state.lock_inner();
            to_value(apply_receipt(&mut inner, &args)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::shared::snapshot;
    use crate::state::AppState;
    use serde_json::json;

    fn state_with_score() -> AppState {
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = oc_dsl::parse(
                "@project\n@track lead | instrument: synth/lead_square\n  @phrase body [1:1 - 2:1]\n    C4 q\n",
            )
            .unwrap();
            inner.dsl_source = oc_dsl::serialize(&inner.project);
        }
        state
    }

    #[test]
    fn compact_mixer_reply_preserves_domain_edit_change_facts_and_snapshot_view() {
        let legacy = state_with_score();
        let compact = state_with_score();
        let mut old = legacy.lock_inner();
        let before = old.project.clone();
        commands::update_track_mix_inner(&mut old, 0, Some(-7.0), Some(0.25)).unwrap();
        let full = serde_json::to_value(snapshot(&old)).unwrap();
        let mut new = compact.lock_inner();
        let receipt = serde_json::to_value(
            apply_receipt(
                &mut new,
                &TracksMixArgs {
                    index: 0,
                    volume_db: Some(-7.0),
                    pan: Some(0.25),
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(old.project, new.project);
        assert_eq!(old.dsl_source, new.dsl_source);
        let change = super::super::describe_edit(&before, &new.project);
        assert!(change.is_some());
        assert_eq!(change, super::super::describe_edit(&before, &old.project));
        assert_eq!(full["project"], serde_json::to_value(&new.project).unwrap());
        assert_eq!(full["dsl_source"], new.dsl_source);
        assert_eq!(receipt["revision"], full["revision"]);
        assert_eq!(receipt["dirty"], full["dirty"]);
        assert_eq!(receipt["track_count"], full["track_count"]);
        assert_eq!(receipt["applied"], true);
        assert_eq!(
            receipt["track"],
            json!({"index":0,"name":"lead","volume_db":-7.0,"pan":0.25})
        );
        assert!(receipt.get("project").is_none());
        assert!(receipt.get("dsl_source").is_none());
        assert!(receipt.to_string().len() < 256);
        assert!(crate::ai::agent_loop::host::publishes_revision(&Ok(
            crate::ai::agent_loop::Dispatched {
                value: receipt,
                changes: change
            }
        )));
    }

    #[test]
    fn invalid_mixer_receipt_is_atomic() {
        for args in [
            TracksMixArgs {
                index: 2,
                volume_db: Some(-7.0),
                pan: None,
            },
            TracksMixArgs {
                index: 0,
                volume_db: Some(f64::NAN),
                pan: None,
            },
            TracksMixArgs {
                index: 0,
                volume_db: None,
                pan: Some(2.0),
            },
        ] {
            let state = state_with_score();
            let mut inner = state.lock_inner();
            let before = serde_json::to_value(snapshot(&inner)).unwrap();
            assert!(apply_receipt(&mut inner, &args).is_err());
            assert_eq!(serde_json::to_value(snapshot(&inner)).unwrap(), before);
        }
    }
}
