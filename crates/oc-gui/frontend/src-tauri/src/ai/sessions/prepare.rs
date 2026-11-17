//! Preparing one turn: deciding the binding, closing a stale round, admitting
//! the request, writing the turn with its host notices, and planning the
//! compaction it needs. Every refusal happens before the first write, so a
//! refused request leaves the store exactly as it was.

use super::super::host_notice::{self, HostNotice, PendingBinding, ToolSurface};
use super::super::session_state::{self, SessionState};
use super::super::session_usage;
use super::journal::{read_view, write_meta};
use super::row::saved_message;
use super::summary::{PlanInputs, plan_compaction_request, should_compact};
use super::{
    AUTO_COMPACT_TARGET_FRACTION, BindingMismatch, BoundProject, CompactionFailure, CompactionPlan,
    HistoryAt, PrepareContext, PrepareFailure, Prepared, SavedHistoryMark, SessionMeta, Store,
    absorb, audition_archive::persist_auditions, close_unanswered_round, estimate_tokens,
    is_audition, now, planned_request, turn_record, turns, versions,
};
use serde_json::Value;
use std::path::Path;

/// What the conversation's metadata owes this turn once the binding is decided.
struct Claimed {
    pending_binding: Option<PendingBinding>,
    pending_state: Option<SessionState>,
    changed: bool,
}

/// The request as it joins the saved conversation: system rows dropped, and
/// any round a dead client left open closed first.
struct Joined {
    incoming: Vec<Value>,
    closed: Vec<Value>,
}

impl Store {
    /// [`Self::prepare_owned`] with an admission check: `admit` sees the
    /// conversation the request would send (after the compaction it would
    /// get) before the archive, the snapshot or the transcript is written, so
    /// a refused request leaves the store exactly as it was.
    pub fn prepare_admitted<E: From<String>>(
        &self,
        id: &str,
        incoming: &[Value],
        project: Option<&Path>,
        context: PrepareContext<'_>,
        incarnation: Option<&str>,
        admit: impl FnOnce(&[Value]) -> Result<(), E>,
    ) -> Result<Prepared, PrepareFailure<E>> {
        let PrepareContext {
            snapshot,
            history,
            overhead,
            context_length,
            max_output,
            prompt_tokens,
            facts,
            provider,
            model,
            outside,
            surface,
        } = context;
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        session_usage::verify_at(&dir, incarnation).map_err(|error| error.message)?;
        // The binding is decided under the store lock, before a transcript,
        // archive or snapshot write, so a refusal leaves every file unchanged.
        let claimed = claim(&mut meta, project, history).map_err(PrepareFailure::Binding)?;
        let mut meta_changed = claimed.changed;
        let (mut pending, _) = read_view(&dir)?;
        let Joined {
            incoming: mut filtered,
            closed,
        } = join(&dir, &mut pending, incoming)?;
        let plan = PlanInputs {
            overhead,
            context_length,
            max_output,
            prompt_tokens,
            facts,
            id,
        };
        admit(&planned_request(&pending, &filtered, &plan)).map_err(PrepareFailure::Admitted)?;

        let audio_settings = crate::gui_settings::load().session_audio;
        let evicted_auditions = persist_auditions(&self.root, &mut filtered, &audio_settings)?;
        let start = pending.len();
        let (appended, absorbed) = absorb(&mut pending, &filtered);
        // The notices the model reads stand before the turn's new message, in
        // the same record, so a reload never shows the text without its cause.
        let (host_rows, surface_changed) = host_rows(&mut meta, claimed, outside, surface);
        meta_changed |= surface_changed;
        let notice_added = !host_rows.is_empty();
        for (offset, row) in host_rows.into_iter().enumerate() {
            pending.insert(start + offset, row);
        }
        // Archiving rewrites auditions, which can move where the request
        // joins the saved conversation, so what is written is checked again.
        // Clips archived for a refused request are collected like any orphan.
        turns::check_pairing(&pending).map_err(|error| format!("sessions: {error}"))?;
        // A version and its first transcript reference share the store lock,
        // so collection cannot remove the version between the two writes.
        versions::attach(&dir, &mut pending[start..], snapshot, project, history)?;
        let inline_audio = filtered
            .iter()
            .any(|message| message.get("input_audio").is_some());
        let closed_round = !closed.is_empty();
        let mut written = None;
        if appended > 0 || closed_round || notice_added {
            let mut added = closed;
            added.extend_from_slice(&pending[start..]);
            written = Some(self.append(&dir, &[turn_record(&now(), added)])?);
        }
        if closed_round {
            // The round is answered on disk, so its started markers are spent.
            // Cleared only after the write: a failed write keeps them, and the
            // next preparation still tells a lost result from a call never run.
            turns::spend_started(&dir, &Default::default());
        }
        let saved_auditions = pending[start..]
            .iter()
            .filter(|message| is_audition(message))
            .cloned()
            .collect();
        // Re-read: the append may have dropped older messages to stay under the
        // cap, and what is sent must be exactly what is saved.
        let (view, _) = read_view(&dir)?;
        let live = inline_audio.then(|| {
            // `absorbed` indexes the client's current-turn history; `start`
            // indexes the saved conversation. A restored session can be much
            // longer than the client tail, so these indices are not interchangeable.
            let mut merged = pending[..start].to_vec();
            merged.extend(filtered.drain(absorbed..));
            merged
        });
        let (pending_compaction, compaction_error) = if inline_audio {
            (None, None)
        } else {
            plan_auto_compaction(&view, &plan)
        };

        if appended > 0
            || pending_compaction.is_some()
            || meta.provider != provider
            || meta.model != model
            || meta_changed
        {
            meta.provider = provider.to_owned();
            meta.model = model.to_owned();
            meta.tokens = written.map_or_else(
                || estimate_tokens(&view),
                |outcome| outcome.kept_tokens(&view),
            );
            if appended > 0 {
                meta.turns += 1;
            }
            meta.updated = now();
            write_meta(&dir, &meta)?;
        }
        Ok(Prepared {
            meta,
            messages: view,
            live,
            appended,
            compacted: None,
            pending_compaction,
            compaction_error,
            evicted_auditions,
            saved_auditions,
        })
    }
}

/// Decide the binding and take what the metadata owes this turn. Every turn
/// path saves the same history mark here, under the store lock and in the
/// same write as the notices: the next turn compares the project against
/// where this one started, not where an earlier one ended.
fn claim(
    meta: &mut SessionMeta,
    project: Option<&Path>,
    history: Option<&HistoryAt>,
) -> Result<Claimed, BindingMismatch> {
    let open = project.map(BoundProject::of);
    let mut changed = false;
    match (&meta.project_path, &open) {
        (Some(bound), Some(open)) if bound != open => {
            return Err(BindingMismatch {
                bound: bound.to_string(),
                open: open.to_string(),
            });
        }
        (None, Some(open)) => {
            // The first turn binds the conversation without telling the
            // model: it has no earlier path to disambiguate.
            meta.project_path = Some(open.clone());
            changed = true;
        }
        _ => {}
    }
    let pending_binding = meta.pending_binding.take();
    let pending_state = meta.pending_state.take();
    changed |= pending_binding.is_some() || pending_state.is_some();
    if let Some(history) = history
        && let project = BoundProject::of(Path::new(&history.project))
        && meta.project_path.as_ref() == Some(&project)
    {
        let saved = SavedHistoryMark::for_project(project, history.at, history.max_id);
        if meta.history_mark.as_ref() != Some(&saved) {
            meta.history_mark = Some(saved);
            changed = true;
        }
    }
    Ok(Claimed {
        pending_binding,
        pending_state,
        changed,
    })
}

/// Join the request to the saved conversation without writing it.
///
/// A window that reloaded mid-turn, or any client that died, leaves the
/// model's last round without results. Closing it here keeps the rule that
/// the store never holds a call no request may carry. A call the request
/// itself answers is not a debt, so the loop's own rounds are left exactly as
/// it sends them. Only the results the request appends count: a client that
/// rebuilds its history resends earlier rounds' results, and a provider that
/// numbers calls per reply reuses their ids for the open round.
fn join(dir: &Path, pending: &mut Vec<Value>, incoming: &[Value]) -> Result<Joined, String> {
    let incoming: Vec<Value> = incoming
        .iter()
        .filter(|message| message["role"] != "system")
        .cloned()
        .collect();
    let fresh = {
        let mut probe = pending.clone();
        absorb(&mut probe, &incoming).1
    };
    let answered: Vec<String> = incoming[fresh.min(incoming.len())..]
        .iter()
        .filter_map(turns::answered_call)
        .collect();
    let answered: Vec<&str> = answered.iter().map(String::as_str).collect();
    let closed = close_unanswered_round(dir, pending, &answered);
    // What a client sends must leave a well-formed conversation, checked
    // before anything is archived or written: one orphan result would refuse
    // every later append of this conversation.
    let mut probe = pending.clone();
    absorb(&mut probe, &incoming);
    turns::check_pairing(&probe).map_err(|error| format!("sessions: {error}"))?;
    Ok(Joined { incoming, closed })
}

/// The host rows this turn saves before its message: the binding, outside and
/// tool-surface notices, then a goal or plan change. Also reports whether the
/// saved tool surface changed.
fn host_rows(
    meta: &mut SessionMeta,
    claimed: Claimed,
    outside: Option<HostNotice>,
    surface: Option<ToolSurface>,
) -> (Vec<Value>, bool) {
    let mut notices = Vec::new();
    if let Some(pending_binding) = claimed.pending_binding
        && let Some(to) = meta.project_path.clone()
    {
        notices.push(HostNotice::ProjectBound {
            from: pending_binding.from,
            to: to.into_string(),
            cause: pending_binding.cause,
        });
    }
    notices.extend(outside);
    let mut surface_changed = false;
    if let Some(surface) = surface {
        if let Some(previous) = &meta.tool_surface
            && let Some(notice) = HostNotice::tool_surface_change(previous, &surface)
        {
            notices.push(notice);
        }
        if meta.tool_surface.as_ref() != Some(&surface) {
            meta.tool_surface = Some(surface);
            surface_changed = true;
        }
    }
    let mut rows: Vec<Value> = notices
        .iter()
        .filter_map(|notice| saved_message(&host_notice::notice_row(notice)))
        .collect();
    rows.extend(
        claimed
            .pending_state
            .and_then(|state| saved_message(&session_state::state_row(&state))),
    );
    (rows, surface_changed)
}

/// The compaction this view needs, planned but not written. A failed plan does
/// not fail the turn: the conversation is sent as saved and admission already
/// bounded it.
fn plan_auto_compaction(
    view: &[Value],
    plan: &PlanInputs<'_>,
) -> (Option<CompactionPlan>, Option<CompactionFailure>) {
    if !should_compact(
        view,
        plan.overhead,
        plan.context_length,
        plan.max_output,
        plan.prompt_tokens,
    ) {
        return (None, None);
    }
    let target = (plan.context_length as f64 * AUTO_COMPACT_TARGET_FRACTION) as u64;
    match plan_compaction_request(view, target, plan.facts, plan.id) {
        Ok(compaction) => (Some(compaction), None),
        Err(error) => {
            log::warn!("[ai] session {} auto-compaction failed: {error}", plan.id);
            (None, Some(CompactionFailure::Store))
        }
    }
}
