//! Application state — single source of truth for the entire DAW session.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use oc_dsl::ast::{self, Project};
use oc_dsl::diff::{self, compressed};
use oc_engine::preset::PresetRegistry;
use oc_engine::render_cache::RenderCache;
use oc_host::PluginInfo;
use oc_host::clap_instance::{ClapInstance, PluginParam};
use oc_host::lv2::Lv2Instance;
use oc_host::vst2::Vst2Instance;
use oc_host::vst3_instance::Vst3Instance;

#[cfg(target_os = "macos")]
use oc_host::au_instance::AuInstance;

use crate::audio::AudioState;
use crate::midi_live::MidiLive;

// ============================================================================
// Undo system — patch-based, scoped, gesture-driven
// ============================================================================

mod automation_history;
mod undo_gestures;
use undo_gestures::GestureOwnership;
pub use undo_gestures::{
    ParameterTargetError, ParameterTargetStamp, UndoGestureError, UndoGestureToken,
};

const MAX_UNDO: usize = 256;
static NEXT_TRANSACTION_ID: AtomicU64 = AtomicU64::new(0);

/// Owner prefix of a batch an AI panel turn opened. The suffix names the
/// conversation, so a window that reloaded can find and recover its own orphan.
pub const PANEL_BATCH_OWNER_PREFIX: &str = "panel:";

/// Owner of a batch an external `/call` or `/mcp` client opened.
pub const HTTP_BATCH_OWNER: &str = "http";

/// Answer of a project replacement while a batch holds pre-commit edits.
pub const TRANSACTION_BLOCKS_REPLACEMENT: &str =
    "commit or roll back the active project transaction before opening another project";

/// Owner prefix of a batch an external client opened for a saved conversation.
/// It is distinct from the panel's prefix, so a panel turn and a headless client
/// bound to the same conversation never join, commit or recover each other's
/// batch.
pub const HTTP_SESSION_OWNER_PREFIX: &str = "http:";

/// The owner a panel turn records on its batch.
pub fn panel_batch_owner(conversation: Option<&str>) -> String {
    format!(
        "{PANEL_BATCH_OWNER_PREFIX}{}",
        conversation.unwrap_or("unsaved")
    )
}

/// The owner an external client's call bound to `conversation` runs as.
pub fn http_session_owner(conversation: &str) -> String {
    format!("{HTTP_SESSION_OWNER_PREFIX}{conversation}")
}

/// The saved conversation a caller acts for, on either surface. The panel's
/// placeholder for a turn that has no conversation yet names none.
pub fn caller_conversation(caller: &str) -> Option<&str> {
    caller
        .strip_prefix(PANEL_BATCH_OWNER_PREFIX)
        .filter(|id| *id != "unsaved")
        .or_else(|| caller.strip_prefix(HTTP_SESSION_OWNER_PREFIX))
        .filter(|id| !id.is_empty())
}

/// A caller outside the panel, whose edits the panel learns about only through
/// a published project snapshot.
pub fn is_external_caller(caller: &str) -> bool {
    caller == HTTP_BATCH_OWNER || caller.starts_with(HTTP_SESSION_OWNER_PREFIX)
}

/// One aspect of the project an edit can touch. Closed on purpose: a new
/// family needs a variant here so merging and the history stay exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoScope {
    Project,
    MixerRouting,
    MixerLevels,
    Preset,
    Automation,
    PluginState,
    Transport,
}

impl UndoScope {
    /// Every undo scope.
    pub const ALL: [UndoScope; 7] = [
        UndoScope::Project,
        UndoScope::MixerRouting,
        UndoScope::MixerLevels,
        UndoScope::Preset,
        UndoScope::Automation,
        UndoScope::PluginState,
        UndoScope::Transport,
    ];

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A small set of scopes, stored as a bitset and serialized as name array.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UndoScopes(u8);

impl UndoScopes {
    /// A set containing exactly one scope.
    pub fn one(scope: UndoScope) -> Self {
        Self(scope.bit())
    }

    /// A set from a list of scopes.
    pub fn of(scopes: &[UndoScope]) -> Self {
        scopes.iter().fold(Self::default(), |mut set, scope| {
            set.insert(*scope);
            set
        })
    }

    /// Add one scope.
    pub fn insert(&mut self, scope: UndoScope) {
        self.0 |= scope.bit();
    }

    /// Union another scope set.
    pub fn insert_all(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Whether the sets share a scope.
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Scopes in declaration order.
    pub fn iter(self) -> impl Iterator<Item = UndoScope> {
        UndoScope::ALL
            .into_iter()
            .filter(move |scope| self.0 & scope.bit() != 0)
    }
}

impl Serialize for UndoScopes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for UndoScopes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let scopes = Vec::<UndoScope>::deserialize(deserializer)?;
        Ok(Self::of(&scopes))
    }
}

/// What one edit touched and how the history names it.
#[derive(Debug, Clone)]
pub struct UndoEdit {
    pub name: String,
    pub scopes: UndoScopes,
    /// Append to the previous entry when the scopes intersect. Gestures merge
    /// unconditionally; outside one this is the only merge request.
    pub merge: bool,
}

impl UndoEdit {
    /// A named edit in one scope.
    pub fn new(name: impl Into<String>, scope: UndoScope) -> Self {
        Self {
            name: name.into(),
            scopes: UndoScopes::one(scope),
            merge: false,
        }
    }

    /// Add another scope to the edit.
    pub fn with_scope(mut self, scope: UndoScope) -> Self {
        self.scopes.insert(scope);
        self
    }

    /// Declare this edit as a continuation of the previous one.
    #[cfg(test)]
    pub fn merge(mut self) -> Self {
        self.merge = true;
        self
    }
}

/// A single entry in the undo stack — stores a compressed forward patch and its inverse.
#[derive(Clone)]
pub struct UndoEntry {
    /// Compressed forward patch (old → new). Stored for potential re-apply (redo).
    pub forward: Vec<u8>,
    /// Compressed inverse patch (new → old). Applied to undo.
    pub inverse: Vec<u8>,
    /// Human-readable name supplied by the caller.
    pub label: String,
    /// Scopes this entry touched.
    pub scopes: UndoScopes,
    /// When the gesture that produced this entry started.
    pub timestamp: SystemTime,
}

/// One level of the history as the UI and the agent panel see it.
#[derive(Debug, Clone, Serialize)]
pub struct UndoHistoryEntry {
    /// Undo level: 1 is the first applied edit; levels above `applied` are redo.
    pub index: usize,
    pub name: String,
    pub scopes: UndoScopes,
    /// Milliseconds since the Unix epoch.
    pub timestamp: u64,
    pub applied: bool,
}

/// Position in the undo/redo history, used by transaction rollback and squash.
#[derive(Debug, Clone, Copy)]
pub struct UndoMark {
    undo: usize,
    redo: usize,
}

/// An open gesture: every push after the first until the matching end merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GestureState {
    scopes: UndoScopes,
    /// Synchronous legacy nesting; owned memberships are tracked separately.
    depth: u32,
    /// Whether a push already opened the entry that later pushes merge into.
    push_started: bool,
}

/// The undo/redo stack, managed entirely on the backend.
pub struct UndoStack {
    pub undo: Vec<UndoEntry>,
    pub redo: Vec<UndoEntry>,
    /// Project before the newest edit; the base for a merged patch.
    merge_base: Option<Project>,
    /// Non-reusing identity for staged history publication; None fails closed.
    history_epoch: Option<u64>,
    /// Open gesture, if any.
    gesture: Option<GestureState>,
    gesture_ownership: GestureOwnership,
    /// Test-only switch: fail patch encoding, which cannot fail otherwise.
    #[cfg(test)]
    fail_encode: bool,
}

impl UndoStack {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            merge_base: None,
            history_epoch: Some(0),
            gesture: None,
            gesture_ownership: GestureOwnership::default(),
            #[cfg(test)]
            fail_encode: false,
        }
    }

    fn join_gesture(&mut self, scope: UndoScope) {
        match &mut self.gesture {
            Some(gesture) => gesture.scopes.insert(scope),
            None => {
                self.gesture = Some(GestureState {
                    scopes: UndoScopes::one(scope),
                    depth: 0,
                    push_started: false,
                });
            }
        }
    }

    /// Synchronous legacy nesting joins the same merge entry as owned gestures.
    pub fn begin_gesture(&mut self, scope: UndoScope) {
        self.join_gesture(scope);
        if let Some(gesture) = &mut self.gesture {
            gesture.depth += 1;
        }
    }

    /// Close only a legacy nesting level; owned memberships are independent.
    pub fn end_gesture(&mut self) {
        if let Some(gesture) = &mut self.gesture {
            gesture.depth = gesture.depth.saturating_sub(1);
        }
        self.close_gesture_if_unowned();
    }

    /// Join the shared merge entry with an exact close owner. Concurrent owners
    /// still merge their edits until all memberships and legacy levels close.
    pub fn begin_owned_gesture(
        &mut self,
        scope: UndoScope,
    ) -> Result<UndoGestureToken, UndoGestureError> {
        let token = self.gesture_ownership.begin()?;
        self.join_gesture(scope);
        Ok(token)
    }

    /// Read-only capture; it never creates a membership or changes undo merging.
    pub fn parameter_target_stamp(&self) -> Option<&ParameterTargetStamp> {
        self.gesture_ownership.parameter_target_stamp()
    }

    pub fn require_parameter_target_stamp(
        &self,
        expected: &ParameterTargetStamp,
    ) -> Result<(), ParameterTargetError> {
        self.gesture_ownership
            .require_parameter_target_stamp(expected)
    }

    /// Mutation admission must check this under the same project mutex as its write.
    pub fn is_owned_gesture_active(&self, token: &UndoGestureToken) -> bool {
        self.gesture_ownership.contains(token)
    }

    /// Unknown, repeated and invalidated tokens leave every live owner intact.
    pub fn end_owned_gesture(&mut self, token: &UndoGestureToken) -> bool {
        if !self.gesture_ownership.close(token) {
            return false;
        }
        self.close_gesture_if_unowned();
        true
    }

    fn close_gesture_if_unowned(&mut self) {
        if self.gesture.is_some_and(|gesture| gesture.depth == 0)
            && !self.gesture_ownership.has_owned()
        {
            self.gesture = None;
        }
    }

    /// Structural edits expire asynchronous owners without consuming legacy nesting.
    pub(crate) fn invalidate_owned_gestures(&mut self) {
        self.advance_history_epoch();
        self.gesture_ownership.invalidate();
        self.close_gesture_if_unowned();
    }

    fn invalidate_gestures(&mut self) {
        self.advance_history_epoch();
        self.gesture = None;
        self.gesture_ownership.invalidate();
    }

    pub fn is_in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    /// Record an edit. Inside a gesture, the first push opens the entry and the
    /// ones that follow merge into it; outside one, only a caller that asked to
    /// merge and whose scopes intersect the newest entry appends to it.
    /// Takes `old` by value so it can be moved into `merge_base` without cloning.
    ///
    /// Returns an error when the patch cannot be encoded: the edit then has no
    /// history level, which the caller reports rather than hides.
    pub fn push(&mut self, old: Project, new: &Project, edit: UndoEdit) -> Result<(), String> {
        self.push_at(old, new, edit, SystemTime::now())
    }

    fn push_at(
        &mut self,
        old: Project,
        new: &Project,
        edit: UndoEdit,
        timestamp: SystemTime,
    ) -> Result<(), String> {
        if undo_gestures::target_topology_changed(&old, new) {
            // Structural publication expires ownership even if encoding fails.
            self.invalidate_owned_gestures();
        }
        let staged = self.stage_push_at(old, new, edit, timestamp)?;
        self.commit_staged_push(staged)
    }

    /// Encode a patch for storage; the test switch makes the error path reachable.
    fn encode(&self, patch: &diff::ProjectPatch) -> Result<Vec<u8>, String> {
        #[cfg(test)]
        if self.fail_encode {
            return Err("test build: patch encoding failed".to_string());
        }
        compressed::encode_patch(patch)
    }

    /// Undo: decode the inverse, apply it, then move the entry to the redo stack.
    ///
    /// A corrupt entry stays on the undo stack and the project is untouched, so
    /// the caller can report that nothing was undone.
    pub fn undo(&mut self, project: &mut Project) -> Result<String, String> {
        let Some(entry) = self.undo.last() else {
            return Err("Nothing to undo".to_string());
        };
        let inverse = compressed::decode_patch(&entry.inverse).map_err(|reason| {
            log::error!(
                "[undo] cannot decode the inverse patch for \"{}\": {reason}",
                entry.label
            );
            format!("could not undo \"{}\": {reason}", entry.label)
        })?;
        diff::apply_patch(project, &inverse)
            .map_err(|reason| format!("could not undo \"{}\": {reason}", entry.label))?;
        let entry = self.undo.pop().expect("the entry was just read");
        let label = entry.label.clone();

        // Move to redo stack (swap forward/inverse for redo)
        self.redo.push(UndoEntry {
            forward: entry.inverse,
            inverse: entry.forward,
            label: format!("Redo: {}", label),
            scopes: entry.scopes,
            timestamp: entry.timestamp,
        });

        // A new edit after this starts a fresh entry, never a merged one.
        self.merge_base = None;
        self.invalidate_gestures();

        Ok(label)
    }

    /// Redo: apply the forward patch (which is stored as inverse in redo entry).
    pub fn redo(&mut self, project: &mut Project) -> Result<String, String> {
        let entry = self
            .redo
            .last()
            .ok_or_else(|| "Nothing to redo".to_string())?;
        let forward = compressed::decode_patch(&entry.inverse)
            .map_err(|reason| format!("could not redo \"{}\": {reason}", entry.label))?;
        diff::apply_patch(project, &forward)
            .map_err(|reason| format!("could not redo \"{}\": {reason}", entry.label))?;
        let entry = self
            .redo
            .pop()
            .expect("the entry was decoded before removal");
        let label = entry.label.clone();

        // Move back to undo stack
        self.undo.push(UndoEntry {
            forward: entry.inverse,
            inverse: entry.forward,
            label: label.strip_prefix("Redo: ").unwrap_or(&label).to_string(),
            scopes: entry.scopes,
            timestamp: entry.timestamp,
        });

        self.merge_base = None;
        self.invalidate_gestures();

        Ok(label)
    }

    /// Move the cursor to `level` applied edits (0 = state before the first
    /// entry), using the stored patches. Returns the name now on top.
    pub fn restore_undo_level(
        &mut self,
        level: usize,
        project: &mut Project,
    ) -> Result<Option<String>, String> {
        let total = self.undo.len() + self.redo.len();
        if level > total {
            return Err(format!(
                "undo level {level} is beyond the {total}-level history"
            ));
        }
        // Decode and apply the entire jump before moving either stack: a later
        // malformed operation must not leave an earlier edit applied.
        let entries: Vec<_> = if self.undo.len() > level {
            self.undo
                .iter()
                .rev()
                .take(self.undo.len() - level)
                .collect()
        } else {
            self.redo
                .iter()
                .rev()
                .take(level - self.undo.len())
                .collect()
        };
        let patches = entries
            .into_iter()
            .map(|entry| {
                compressed::decode_patch(&entry.inverse).map_err(|reason| {
                    format!("could not restore history at \"{}\": {reason}", entry.label)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        diff::apply_patches(project, &patches)
            .map_err(|reason| format!("could not restore history: {reason}"))?;
        while self.undo.len() > level {
            let entry = self.undo.pop().expect("history length was checked");
            self.redo.push(UndoEntry {
                forward: entry.inverse,
                inverse: entry.forward,
                label: format!("Redo: {}", entry.label),
                scopes: entry.scopes,
                timestamp: entry.timestamp,
            });
        }
        while self.undo.len() < level {
            let entry = self.redo.pop().expect("history length was checked");
            self.undo.push(UndoEntry {
                forward: entry.inverse,
                inverse: entry.forward,
                label: entry
                    .label
                    .strip_prefix("Redo: ")
                    .unwrap_or(&entry.label)
                    .to_string(),
                scopes: entry.scopes,
                timestamp: entry.timestamp,
            });
        }
        self.merge_base = None;
        self.invalidate_gestures();
        Ok(self.undo.last().map(|entry| entry.label.clone()))
    }

    /// Ordered history: applied entries oldest first, then the redo levels.
    pub fn history(&self) -> Vec<UndoHistoryEntry> {
        let applied = self.undo.len();
        self.undo
            .iter()
            .enumerate()
            .map(|(offset, entry)| Self::history_entry(offset + 1, entry, true))
            .chain(
                self.redo
                    .iter()
                    .rev()
                    .enumerate()
                    .map(|(offset, entry)| Self::history_entry(applied + offset + 1, entry, false)),
            )
            .collect()
    }

    fn history_entry(index: usize, entry: &UndoEntry, applied: bool) -> UndoHistoryEntry {
        UndoHistoryEntry {
            index,
            name: entry
                .label
                .strip_prefix("Redo: ")
                .unwrap_or(&entry.label)
                .to_string(),
            scopes: entry.scopes,
            timestamp: entry
                .timestamp
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as u64)
                .unwrap_or(0),
            applied,
        }
    }

    /// Get labels for the undo history (most recent first).
    pub fn undo_labels(&self) -> Vec<String> {
        self.undo.iter().rev().map(|e| e.label.clone()).collect()
    }

    /// Get labels for the redo history (most recent first).
    pub fn redo_labels(&self) -> Vec<String> {
        self.redo.iter().rev().map(|e| e.label.clone()).collect()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn mark(&self) -> UndoMark {
        UndoMark {
            undo: self.undo.len(),
            redo: self.redo.len(),
        }
    }

    /// Drop edits recorded after `mark`, e.g. when a transaction rolls back.
    pub fn restore(&mut self, mark: UndoMark) {
        self.undo.truncate(mark.undo.min(self.undo.len()));
        self.redo.truncate(mark.redo.min(self.redo.len()));
        self.merge_base = None;
        self.invalidate_gestures();
    }

    /// Replace the edits recorded after `mark` with one combined entry.
    pub fn squash_since(&mut self, mark: UndoMark, old: Project, new: &Project, edit: UndoEdit) {
        let cut = mark.undo.min(self.undo.len());
        let mut scopes = edit.scopes;
        for entry in &self.undo[cut..] {
            scopes.insert_all(entry.scopes);
        }
        self.undo.truncate(cut);
        self.redo.clear();
        match diff::diff(&old, new) {
            Ok(Some(patch)) => {
                let inverse = diff::invert_patch(&old, new, &patch);
                match (self.encode(&patch), self.encode(&inverse)) {
                    (Ok(forward), Ok(inverse)) => {
                        self.undo.push(UndoEntry {
                            forward,
                            inverse,
                            label: edit.name,
                            scopes,
                            timestamp: SystemTime::now(),
                        });
                        if self.undo.len() > MAX_UNDO {
                            self.undo.remove(0);
                        }
                    }
                    // The batch stays committed; its history entry is what failed.
                    _ => log::error!("[undo] cannot encode the transaction history entry"),
                }
            }
            Ok(None) => {}
            Err(error) => {
                log::error!("[undo] cannot compare the transaction history: {error}");
            }
        }
        self.merge_base = Some(old);
        self.invalidate_gestures();
    }

    pub fn clear(&mut self) {
        self.advance_history_epoch();
        self.undo.clear();
        self.redo.clear();
        self.merge_base = None;
        self.gesture = None;
        self.gesture_ownership.replace_document();
    }
}

// ============================================================================
// App state
// ============================================================================

/// Orders whole-patch loads. A click that lands while another load is still
/// running supersedes it, so the newest patch is the one that is applied.
#[derive(Default)]
pub struct PatchLoadGate {
    issued: AtomicU64,
}

impl PatchLoadGate {
    /// Claim the newest request. The ticket identifies it for [`Self::superseded`].
    pub fn begin(&self) -> u64 {
        self.issued.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// True when a later request has already claimed the load.
    pub fn superseded(&self, ticket: u64) -> bool {
        self.issued.load(Ordering::SeqCst) > ticket
    }
}

/// A VST3 instance that is not in the playback graph: the editor preview, its
/// parameter panel and its state capture share it.
///
/// The instance has its own lock so a keep-alive block or a parameter read
/// never holds the document mutex. The document lock is always taken first
/// when both are needed; nothing may take them the other way round.
pub type Vst3Preview = Arc<Mutex<Vst3Instance>>;

/// Lock a preview instance, recovering from a poisoned mutex.
pub fn lock_vst3_preview(preview: &Vst3Preview) -> MutexGuard<'_, Vst3Instance> {
    preview.lock().unwrap_or_else(|error| error.into_inner())
}

/// Parameter inventory last handed to the UI, per preset.
///
/// Enumerating is the expensive part of a plugin poll — a documented
/// 737-parameter plugin costs a count plus an info record and a value per
/// parameter — so the list is kept until the plugin reports a change or the
/// caller refreshes it on purpose.
#[derive(Default)]
pub struct ParamCache {
    entries: HashMap<String, Vec<PluginParam>>,
}

impl ParamCache {
    /// The cached inventory, or `None` when it must be enumerated again.
    pub fn get(&self, preset: &str) -> Option<&[PluginParam]> {
        self.entries.get(preset).map(Vec::as_slice)
    }

    /// Keep an inventory for the next poll, replacing any older one.
    pub fn store(&mut self, preset: &str, params: Vec<PluginParam>) {
        self.entries.insert(preset.to_owned(), params);
    }

    /// Drop one preset's inventory; a reported change invalidates it.
    pub fn invalidate(&mut self, preset: &str) {
        self.entries.remove(preset);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

pub struct AppState {
    pub(crate) preview_focus: crate::preview_focus::PreviewFocus,
    /// Linearizes project-load request acceptance and final replacement.
    pub(crate) project_load_epoch: Mutex<u64>,
    pub(crate) transport_pause_intent: Arc<AtomicBool>,
    pub(crate) audition_pause_intent: Arc<AtomicBool>,
    pub inner: Mutex<AppStateInner>,
    /// Coalesces rapid patch clicks so only the newest one mutates the project.
    pub patch_load: PatchLoadGate,
    /// Off-thread preset audition, its instance cache and readiness status.
    pub audition: crate::audition::AuditionController,
    /// Level-matched A/B pair rendered from two candidate patches.
    pub ab: crate::ab_audition::AbController,
    /// Serialises agent range renders without blocking the editor or UI render.
    pub range_render_lock: Mutex<()>,
    /// Serialises plugin instantiation so two loads do not race for the same module.
    pub plugin_load_lock: Mutex<()>,
    /// Full render reused by `render_range`; keyed by content epoch and rate.
    pub range_cache: Mutex<Option<RangeCacheEntry>>,
    /// Pre-transaction render kept while a batch is open; a rollback serves it.
    pub range_cache_hold: Mutex<Option<RangeCacheEntry>>,
    /// Narrow agent windows per project revision; an agent-session property.
    pub window_guard: Mutex<crate::window_guard::WindowGuard>,
    /// Set by the export dialog's Cancel; checkpoints between render stages read
    /// the token of the export that is current and drop its staged file instead
    /// of publishing it. Each export claims its own token, so a Cancel never
    /// stops an export that is already running beside it.
    export_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Receipts remain owned by actual native workers, independently of awaiters.
    pub(crate) native_jobs: Arc<crate::native_jobs::NativeJobs>,
}

pub struct RangeCacheEntry {
    /// Content epoch of the project state this render was made from.
    pub epoch: u64,
    pub sample_rate: u32,
    pub cache: RenderCache,
}

impl AppState {
    /// Lock the inner state, recovering from mutex poisoning.
    pub fn lock_inner(&self) -> MutexGuard<'_, AppStateInner> {
        self.inner.lock().unwrap_or_else(|e| {
            log::warn!("Recovering from poisoned mutex (a previous command panicked)");
            e.into_inner()
        })
    }

    /// Lock the inner state only if no command holds it.
    ///
    /// The editor keep-alive refreshes its clock through this: a preview block
    /// must never wait for the document, so a busy document means the block
    /// keeps the values it already has.
    pub fn try_lock_inner(&self) -> Option<MutexGuard<'_, AppStateInner>> {
        self.inner.try_lock().ok()
    }

    #[cfg(test)]
    pub fn audio_simulated(&self) -> bool {
        self.lock_inner().audio.simulated_output()
    }

    /// Refuse a document mutation while an edit preview holds the document.
    pub fn ensure_writable(&self) -> Result<(), String> {
        self.lock_inner().refuse_in_preview()
    }

    /// Refuse a whole-document replacement while a batch holds pre-commit edits.
    pub fn refuse_in_transaction(&self) -> Result<(), String> {
        self.lock_inner().refuse_in_transaction()
    }

    pub fn lock_range_render(&self) -> MutexGuard<'_, ()> {
        self.range_render_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn try_lock_plugin_load(&self) -> Option<MutexGuard<'_, ()>> {
        self.plugin_load_lock.try_lock().ok()
    }

    pub fn lock_plugin_load(&self) -> MutexGuard<'_, ()> {
        self.plugin_load_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// The agent window counter; locked per admission, never over a render.
    pub fn window_guard(&self) -> MutexGuard<'_, crate::window_guard::WindowGuard> {
        self.window_guard.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claim the cancel token for the export that starts now.
    ///
    /// The token is fresh, so a request that arrived after the previous export
    /// finished cannot stop this one. A later export replaces the slot; the
    /// token an earlier export still holds stays untouched.
    pub fn begin_export_cancel(&self) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        *self.export_cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&cancel));
        cancel
    }

    /// The dialog's Cancel: the current export stops at its next checkpoint and
    /// drops its staged file. Safe from any thread; no document lock is taken.
    pub fn request_export_cancel(&self) {
        let slot = self.export_cancel.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cancel) = slot.as_ref() {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    fn lock_range_cache(&self) -> MutexGuard<'_, Option<RangeCacheEntry>> {
        self.range_cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_range_cache_hold(&self) -> MutexGuard<'_, Option<RangeCacheEntry>> {
        self.range_cache_hold
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Remove the cached full render when it matches the requested state.
    pub fn take_range_cache(&self, epoch: u64, sample_rate: u32) -> RenderCache {
        match self.lock_range_cache().take() {
            Some(entry) if entry.epoch == epoch && entry.sample_rate == sample_rate => entry.cache,
            _ => RenderCache::new(),
        }
    }

    /// Keep a render for later windows only while the project is unchanged.
    pub fn store_range_cache(&self, epoch: u64, sample_rate: u32, cache: RenderCache) {
        if self.lock_inner().content_epoch != epoch {
            return;
        }
        self.store_range_cache_locked(epoch, sample_rate, cache);
    }

    /// Store under a publish decision the caller already made with `inner` held.
    pub fn store_range_cache_locked(&self, epoch: u64, sample_rate: u32, cache: RenderCache) {
        *self.lock_range_cache() = Some(RangeCacheEntry {
            epoch,
            sample_rate,
            cache,
        });
    }

    /// Open a batch and keep its pre-transaction render aside for a rollback.
    pub fn begin_transaction(&self, label: String, owner: String) -> Result<u64, String> {
        let base = self.lock_inner().begin_transaction(label, owner)?;
        let current = self.lock_range_cache().take();
        *self.lock_range_cache_hold() = current;
        Ok(base)
    }

    /// The owner of the open batch, or `None` when no batch is open.
    pub fn batch_owner(&self) -> Option<String> {
        self.lock_inner()
            .transaction
            .as_ref()
            .map(|transaction| transaction.owner.clone())
    }

    /// Close a batch, publishing it; the pre-transaction render is now stale.
    pub fn commit_transaction(&self) -> Result<(String, u64), String> {
        let committed = self.lock_inner().commit_transaction()?;
        *self.lock_range_cache_hold() = None;
        Ok(committed)
    }

    /// Restore the pre-transaction project and its render.
    pub fn rollback_transaction(&self) -> Result<(String, u64), String> {
        let rolled_back = self.lock_inner().rollback_transaction()?;
        let held = self.lock_range_cache_hold().take();
        *self.lock_range_cache() = held;
        Ok(rolled_back)
    }

    /// Drop a held pre-transaction render, e.g. when another project replaces it.
    pub fn discard_range_cache_hold(&self) {
        *self.lock_range_cache_hold() = None;
    }
}

/// What the operator chose on the start surface. `None` means nothing is chosen
/// yet, so a project's directory is still the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceChoice {
    /// A directory: it scopes the saved conversations, with or without an open
    /// project.
    Folder(PathBuf),
    /// Continue without a workspace. A project opened later scopes the
    /// conversations to its directory, as before.
    Detached,
}

pub struct AppStateInner {
    pub project: Project,
    pub dsl_source: String,
    pub file_path: Option<PathBuf>,
    /// Identifies this app session. Recovery copies of a never-saved document
    /// are keyed by it, so two open windows never share a directory.
    pub session_id: String,
    /// The workspace this run works in, once the operator has chosen one.
    pub workspace: Option<WorkspaceChoice>,
    pub dirty: bool,
    /// The operator answered Don't Save: the document must never reach the file.
    /// An edit or a project load clears it, because both are about a document
    /// the answer no longer covers.
    pub discard_pending: bool,
    /// `quit_now` ran: the exit flush may publish one last time.
    pub exit_ready: bool,
    /// When the recovery writer last started its interval or wrote a copy.
    pub autosave_at: Option<std::time::Instant>,
    /// Digest of the source last written to a recovery copy.
    pub autosave_digest: u64,
    /// Monotonic project version. Every committed mutation bumps it.
    pub revision: u64,
    /// Render-cache key of the project content. It also moves on an edit inside
    /// a transaction, and a rollback restores the pre-transaction value.
    pub content_epoch: u64,
    /// Newest offline render started by this session; older renders drop out.
    pub render_generation: u64,
    /// Snapshot captured by `transaction_begin`; `None` when no batch is open.
    pub transaction: Option<ProjectTransaction>,
    pub audio: AudioState,
    pub presets: PresetRegistry,
    /// Live CLAP plugin instances, keyed by preset name.
    pub plugin_instances: HashMap<String, ClapInstance>,
    /// Whether an out-of-process probe found a plugin editor. A preset whose
    /// parameter table came from a probe has no live instance yet, so the
    /// panel's "Open editor" button still has to know the answer.
    pub plugin_gui_hints: HashMap<String, bool>,
    /// Live VST3 instances used by native editors and exact state capture.
    /// Each one carries its own lock; the document lock is never held across
    /// its DSP.
    pub vst3_plugin_instances: HashMap<String, Vst3Preview>,
    /// Live VST2 instances used by parameter editing and state capture.
    pub vst2_plugin_instances: HashMap<String, Vst2Instance>,
    /// Live LV2 instances used by the generated parameter panel and state capture.
    pub lv2_plugin_instances: HashMap<String, Lv2Instance>,
    /// Live AudioUnit instances used by the generated parameter panel.
    #[cfg(target_os = "macos")]
    pub au_plugin_instances: HashMap<String, AuInstance>,
    /// Silent-block keep-alive threads for VST3 editors that are open.
    pub vst3_editor_keepalives: HashMap<String, crate::plugin_keepalive::EditorKeepAlive>,
    /// One undo transaction per live plugin-parameter drag/recording gesture.
    pub plugin_automation_gestures: HashMap<String, PluginAutomationGesture>,
    /// Parameter inventories already sent to the UI, per preset.
    pub plugin_param_cache: ParamCache,
    /// Recording memberships keyed by panel target, chain position and parameter;
    /// begin snapshots support final audio synchronization, not history replay.
    pub effect_write_gestures: HashMap<String, EffectWriteGesture>,
    /// The project as it stood when a mixer fader was grabbed. Live updates
    /// move the AST and the graph; the release commits against this snapshot.
    pub mixer_touch: Option<MixerTouch>,
    /// CLAP editors successfully created by this session; destroy exactly once.
    pub open_clap_editors: HashSet<String>,
    /// Out-of-process editor hosts, keyed by preset name.
    pub editor_helpers: HashMap<String, crate::editor_helper::EditorHelper>,
    /// Cached scan results from startup (avoids re-scanning)
    pub scanned_plugins: Vec<PluginInfo>,
    /// Backend-managed undo/redo stack (patch-based)
    pub undo: UndoStack,
    /// The open edit preview: the base and the proposal, one of them live.
    /// While it is open the document is read-only.
    pub preview: Option<crate::preview::PreviewSession>,
    /// Live MIDI selection plus the polling thread (not part of audio config).
    pub midi_live: MidiLive,
    /// Shared with the audition cache; every revision bump invalidates it.
    audition_epoch: Arc<AtomicU64>,
}

pub struct EffectWriteGesture {
    /// Internal close/checkpoint membership, including tokenless legacy IPC.
    pub history_owner: UndoGestureToken,
    pub owner: Option<UndoGestureToken>,
    pub old_project: Project,
}

pub struct PluginAutomationGesture {
    /// Internal close/checkpoint membership, including tokenless legacy IPC.
    pub history_owner: UndoGestureToken,
    /// None selects the tokenless legacy endpoint protocol.
    pub owner: Option<UndoGestureToken>,
    pub old_project: Project,
    pub track_index: usize,
    pub lane_param: String,
}

/// One open mixer fader drag: its target (`track:N` or `master`) and the
/// document the pointer grabbed.
pub struct MixerTouch {
    pub target: String,
    pub old_project: Project,
}

/// Pre-transaction snapshot. Rollback restores the project, DSL text, dirty
/// flag, path and undo history; commit squashes the batch into one undo entry.
pub struct ProjectTransaction {
    pub(crate) id: u64,
    pub(crate) handoff_media: Vec<crate::commands::HandoffMedia>,
    project: Project,
    dsl_source: String,
    dirty: bool,
    file_path: Option<PathBuf>,
    undo_mark: UndoMark,
    /// Entries the batch undid from below its mark, newest first, so a rollback
    /// can put the pre-batch history back.
    base_undos: Vec<UndoEntry>,
    pub label: String,
    /// The caller that opened the batch: `panel:<conversation>` or `http`. A
    /// caller that does not own the open batch may neither edit nor close it.
    pub owner: String,
    pub base_revision: u64,
    /// Content epoch at `begin`; a rollback restores it so the held render fits.
    base_epoch: u64,
    /// Set by the first mutation inside the batch; the commit owns the bump.
    changed: bool,
}

impl AppState {
    /// A test state whose audio subsystem drives an explicit device backend.
    #[cfg(test)]
    pub(crate) fn with_audio_backend(
        backend: std::sync::Arc<dyn oc_audio::audio_io::DeviceBackend>,
    ) -> Self {
        let state = Self::new();
        let mut audio = AudioState::with_backend(backend);
        audio.enable_simulated_output();
        audio.audition_pause_intent = Arc::clone(&state.audition_pause_intent);
        audio.transport_pause_intent = Arc::clone(&state.transport_pause_intent);
        {
            let mut inner = state.lock_inner();
            audio.set_mpe_config(inner.midi_live.view().mpe);
            inner.audio = audio;
        }
        state
    }

    pub fn new() -> Self {
        let project = default_project();
        let dsl_source = oc_dsl::serialize(&project);
        let (audition, audition_epoch) = crate::audition::AuditionController::new();
        let mut presets = PresetRegistry::new();

        let plugins = Vec::new();
        presets.register_clap_plugins(&plugins);

        let mut audio = AudioState::new();
        // Tests must never open the operator's audio device.
        #[cfg(test)]
        audio.enable_simulated_output();
        let mut midi_live = MidiLive::default();
        midi_live.ensure_loaded();
        audio.set_mpe_config(midi_live.view().mpe);
        Self {
            transport_pause_intent: Arc::clone(&audio.transport_pause_intent),
            audition_pause_intent: Arc::clone(&audio.audition_pause_intent),
            project_load_epoch: Mutex::new(0),
            preview_focus: crate::preview_focus::PreviewFocus::default(),
            patch_load: PatchLoadGate::default(),
            range_render_lock: Mutex::new(()),
            plugin_load_lock: Mutex::new(()),
            range_cache: Mutex::new(None),
            range_cache_hold: Mutex::new(None),
            window_guard: Mutex::new(crate::window_guard::WindowGuard::default()),
            export_cancel: Mutex::new(None),
            native_jobs: Arc::new(crate::native_jobs::NativeJobs::default()),
            audition,
            ab: crate::ab_audition::AbController::new(),
            inner: Mutex::new(AppStateInner {
                project,
                dsl_source,
                file_path: None,
                session_id: crate::autosave::new_session_id(),
                workspace: None,
                dirty: false,
                discard_pending: false,
                exit_ready: false,
                autosave_at: None,
                autosave_digest: 0,
                revision: 0,
                content_epoch: 0,
                render_generation: 0,
                transaction: None,
                audio,
                presets,
                plugin_instances: HashMap::new(),
                plugin_gui_hints: HashMap::new(),
                vst3_plugin_instances: HashMap::new(),
                vst2_plugin_instances: HashMap::new(),
                lv2_plugin_instances: HashMap::new(),
                #[cfg(target_os = "macos")]
                au_plugin_instances: HashMap::new(),
                vst3_editor_keepalives: HashMap::new(),
                plugin_automation_gestures: HashMap::new(),
                plugin_param_cache: ParamCache::default(),
                effect_write_gestures: HashMap::new(),
                mixer_touch: None,
                open_clap_editors: HashSet::new(),
                editor_helpers: HashMap::new(),
                scanned_plugins: plugins,
                undo: UndoStack::new(),
                preview: None,
                midi_live,
                audition_epoch,
            }),
        }
    }
}

impl AppStateInner {
    /// Bump the project revision after a mutation or a rollback. Inside an
    /// agent batch the first edit marks the batch and the commit bumps once.
    ///
    /// A mutation is also a new answer on the quit prompt: a discard covers the
    /// document it was given for, not the one this edit makes.
    pub fn bump_revision(&mut self) {
        self.discard_pending = false;
        self.audition_epoch.fetch_add(1, Ordering::SeqCst);
        self.content_epoch = self.content_epoch.wrapping_add(1);
        if let Some(transaction) = &mut self.transaction {
            transaction.changed = true;
            return;
        }
        self.revision = self.revision.saturating_add(1);
    }

    pub(crate) fn invalidate_parameter_owners(&mut self) {
        self.undo.invalidate_owned_gestures();
        self.audio.retire_owned_effect_touches();
    }

    pub(crate) fn retire_touches_for_topology_change(&self, old: &ast::Project) {
        if undo_gestures::target_topology_changed(old, &self.project) {
            self.audio.retire_owned_effect_touches();
        }
    }

    /// Rebuild the DSL and the audio graph after history restored the project,
    /// without recording a new undo entry.
    pub fn resync_after_history(&mut self) {
        self.audio.retire_owned_effect_touches();
        self.audio.end_live_synth();
        self.audio.resource_root = self
            .file_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        oc_dsl::instruments::apply_defaults(&mut self.project);
        self.dsl_source = oc_dsl::serialize(&self.project);
        self.dirty = true;
        self.audio.mark_dirty();
        self.audio.preload_project(&self.project);
        self.bump_revision();
    }

    /// Undo the newest edit, whether the open batch made it or it predates the
    /// batch. Entries below the batch's mark are kept, so a rollback still
    /// restores the history the batch found.
    pub fn undo_step(&mut self) -> Result<String, String> {
        self.refuse_in_preview()?;
        let below_mark = self
            .transaction
            .as_ref()
            .is_some_and(|transaction| self.undo.undo.len() <= transaction.undo_mark.undo);
        let base_entry = below_mark.then(|| self.undo.undo.last().cloned()).flatten();
        let mut project = self.project.clone();
        let label = self.undo.undo(&mut project)?;
        if let (Some(entry), Some(transaction)) = (base_entry, self.transaction.as_mut()) {
            transaction.base_undos.push(entry);
        }
        self.project = project;
        self.resync_after_history();
        Ok(label)
    }

    /// Claim the newest render slot; an older render must not publish over it.
    pub fn begin_offline_render(&mut self) -> u64 {
        self.render_generation = self.render_generation.wrapping_add(1);
        self.render_generation
    }

    pub fn begin_transaction(&mut self, label: String, owner: String) -> Result<u64, String> {
        self.refuse_in_preview()?;
        if self.transaction.is_some() {
            return Err("a project transaction is already active".into());
        }
        let base_revision = self.revision;
        self.transaction = Some(ProjectTransaction {
            id: NEXT_TRANSACTION_ID.fetch_add(1, Ordering::Relaxed),
            handoff_media: Vec::new(),
            project: self.project.clone(),
            dsl_source: self.dsl_source.clone(),
            dirty: self.dirty,
            file_path: self.file_path.clone(),
            undo_mark: self.undo.mark(),
            base_undos: Vec::new(),
            label,
            owner,
            base_revision,
            base_epoch: self.content_epoch,
            changed: false,
        });
        Ok(base_revision)
    }

    /// Keep the batch, publish exactly one revision and collapse its undo
    /// entries into one transaction entry.
    pub fn commit_transaction(&mut self) -> Result<(String, u64), String> {
        let mut transaction = self
            .transaction
            .take()
            .ok_or_else(|| "no active project transaction".to_string())?;
        self.undo.squash_since(
            transaction.undo_mark,
            transaction.project,
            &self.project,
            UndoEdit::new(
                format!("Transaction: {}", transaction.label),
                UndoScope::Project,
            ),
        );
        if transaction.changed {
            self.revision = self.revision.saturating_add(1);
            self.audition_epoch.fetch_add(1, Ordering::SeqCst);
            // The published state is a new content key, not the batch's last edit.
            self.content_epoch = self.content_epoch.wrapping_add(1);
        }
        for media in &mut transaction.handoff_media {
            media.retain();
        }
        Ok((transaction.label, self.revision))
    }

    /// Restore the pre-transaction project and undo history.
    pub fn rollback_transaction(&mut self) -> Result<(String, u64), String> {
        let transaction = self
            .transaction
            .take()
            .ok_or_else(|| "no active project transaction".to_string())?;
        self.audio.retire_owned_effect_touches();
        self.project = transaction.project;
        self.dsl_source = transaction.dsl_source;
        self.dirty = transaction.dirty;
        self.file_path = transaction.file_path;
        // Keep the base entries the batch undid, then drop what the batch added.
        let keep = transaction
            .undo_mark
            .undo
            .saturating_sub(transaction.base_undos.len());
        self.undo.restore(UndoMark {
            undo: keep,
            redo: transaction.undo_mark.redo,
        });
        for entry in transaction.base_undos.into_iter().rev() {
            self.undo.undo.push(entry);
        }
        self.audio.resource_root = self
            .file_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        self.audio.mark_dirty();
        self.audio.preload_project(&self.project);
        if transaction.changed {
            self.bump_revision();
            // The restored project is the pre-batch state, so its render fits again.
            self.content_epoch = transaction.base_epoch;
        }
        Ok((transaction.label, self.revision))
    }

    /// Reject a mutation that was planned against an older project version.
    /// Inside a transaction the revision returned by `begin` is also accepted.
    pub fn check_revision(&self, expected: Option<u64>) -> Result<(), String> {
        let Some(expected) = expected else {
            return Ok(());
        };
        if expected == self.revision {
            return Ok(());
        }
        if self
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.base_revision == expected)
        {
            return Ok(());
        }
        Err(format!(
            "stale revision {expected}: project is at revision {}",
            self.revision
        ))
    }

    /// Refuse a document mutation while an edit preview holds the document.
    ///
    /// The preview keeps two documents; a mutation would land on whichever side
    /// is on screen and make the comparison meaningless, so every mutating
    /// command answers with the same sentence the panel shows.
    pub fn refuse_in_preview(&self) -> Result<(), String> {
        match self.preview {
            Some(_) => Err(crate::preview::READ_ONLY.to_string()),
            None => Ok(()),
        }
    }

    /// Refuse a whole-document replacement while a batch holds pre-commit edits.
    ///
    /// The batch's edits live only in memory until it commits, so replacing the
    /// document would drop them without a recovery copy and leave the agent
    /// committing against a document that is gone.
    pub fn refuse_in_transaction(&self) -> Result<(), String> {
        match self.transaction {
            Some(_) => Err(TRANSACTION_BLOCKS_REPLACEMENT.to_string()),
            None => Ok(()),
        }
    }

    /// Whether an edit preview currently holds the document.
    pub fn previewing(&self) -> bool {
        self.preview.is_some()
    }

    /// Stop every out-of-process editor helper.
    ///
    /// A helper owns a separate plugin instance loaded from this project, so it
    /// belongs to the document. A replacement, a close and the exit path all
    /// call this; nothing the helper opened outlives the project.
    pub fn close_editor_helpers(&mut self) {
        for (_, helper) in self.editor_helpers.drain() {
            helper.kill();
        }
    }
}

fn default_project() -> Project {
    Project {
        name: Some("Untitled".to_string()),
        ocp_version: Some(oc_dsl::DSL_VERSION.to_string()),
        properties: ast::ProjectProperties::default(),
        tracks: vec![],
        patterns: vec![],
        presets: vec![],
        buses: vec![],
        master: None,
        sections: vec![],
        markers: Vec::new(),
        groups: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_owners_name_their_surface_and_conversation() {
        let panel = panel_batch_owner(Some("abc"));
        let bound = http_session_owner("abc");
        assert_ne!(panel, bound);
        assert_eq!(caller_conversation(&panel), Some("abc"));
        assert_eq!(caller_conversation(&bound), Some("abc"));
        assert_eq!(caller_conversation(&panel_batch_owner(None)), None);
        assert_eq!(caller_conversation(HTTP_BATCH_OWNER), None);
        assert_eq!(caller_conversation("http:"), None);
        assert!(is_external_caller(HTTP_BATCH_OWNER));
        assert!(is_external_caller(&bound));
        assert!(!is_external_caller(&panel));
    }

    fn invalid_reorder_patch(project: &Project) -> Vec<u8> {
        let mut properties = project.properties.clone();
        properties.tempo = 999.0;
        compressed::encode_patch(&diff::ProjectPatch {
            label: "Invalid reorder after an otherwise valid edit".into(),
            changes: vec![
                diff::PatchOp::SetProperties(properties),
                diff::PatchOp::ReorderTracks(vec!["missing track".into()]),
            ],
        })
        .unwrap()
    }

    #[test]
    fn checked_history_application_refuses_invalid_undo_and_redo_without_moving_entries() {
        for redo in [false, true] {
            let mut stack = UndoStack::new();
            let mut project = default_project();
            stack_edit(
                &mut stack,
                &mut project,
                "tempo",
                UndoScope::Transport,
                150.0,
            );
            if redo {
                stack.undo(&mut project).unwrap();
            }
            let bad = invalid_reorder_patch(&project);
            if redo {
                stack.redo.last_mut().unwrap().inverse = bad.clone();
            } else {
                stack.undo.last_mut().unwrap().inverse = bad.clone();
            }
            stack.begin_gesture(UndoScope::Project);
            let before = project.clone();
            let history = serde_json::to_value(stack.history()).unwrap();
            let result = if redo {
                stack.redo(&mut project)
            } else {
                stack.undo(&mut project)
            };
            assert!(result.is_err());
            assert_eq!(project, before);
            assert_eq!(serde_json::to_value(stack.history()).unwrap(), history);
            assert!(stack.is_in_gesture());
            let entry = if redo {
                stack.redo.last().unwrap()
            } else {
                stack.undo.last().unwrap()
            };
            assert_eq!(entry.inverse, bad);
        }
    }

    #[test]
    fn checked_history_jump_refuses_a_later_invalid_operation_without_applying_earlier_patches() {
        for redo in [false, true] {
            let mut stack = UndoStack::new();
            let mut project = default_project();
            for tempo in [130.0, 150.0] {
                stack_edit(
                    &mut stack,
                    &mut project,
                    "tempo",
                    UndoScope::Transport,
                    tempo,
                );
            }
            if redo {
                stack.restore_undo_level(0, &mut project).unwrap();
            }
            let bad = invalid_reorder_patch(&project);
            if redo {
                stack.redo[0].inverse = bad;
            } else {
                stack.undo[0].inverse = bad;
            }
            let before = project.clone();
            let history = serde_json::to_value(stack.history()).unwrap();
            assert!(
                stack
                    .restore_undo_level(if redo { 2 } else { 0 }, &mut project)
                    .is_err()
            );
            assert_eq!(project, before);
            assert_eq!(serde_json::to_value(stack.history()).unwrap(), history);
        }
    }

    fn test_inner() -> AppStateInner {
        let state = AppState::new();
        state.inner.into_inner().unwrap()
    }

    fn edit_tempo(inner: &mut AppStateInner, tempo: f64) {
        let mut project = inner.project.clone();
        project.properties.tempo = tempo;
        let old = std::mem::replace(&mut inner.project, project);
        inner
            .undo
            .push(
                old,
                &inner.project,
                UndoEdit::new("tempo", UndoScope::Transport),
            )
            .expect("the test edit records");
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.dirty = true;
        inner.bump_revision();
    }

    #[test]
    fn rollback_restores_project_dsl_dirty_and_undo() {
        let mut inner = test_inner();
        let base_project = inner.project.clone();
        let base_dsl = inner.dsl_source.clone();
        let base_revision = inner.revision;
        assert_eq!(
            inner
                .begin_transaction("batch".into(), "test".into())
                .unwrap(),
            base_revision
        );
        edit_tempo(&mut inner, 142.0);
        assert_eq!(inner.undo.undo.len(), 1);
        assert_ne!(inner.project, base_project);

        let (label, revision) = inner.rollback_transaction().unwrap();
        assert_eq!(label, "batch");
        assert_eq!(revision, base_revision + 1);
        assert_eq!(inner.project, base_project);
        assert_eq!(inner.dsl_source, base_dsl);
        assert!(!inner.dirty);
        assert!(inner.undo.undo.is_empty());
        assert!(inner.transaction.is_none());
        assert!(inner.rollback_transaction().is_err());
    }

    #[test]
    fn rollback_is_byte_identical_and_keeps_one_entry_per_batch() {
        let mut inner = test_inner();
        let base_bytes = oc_dsl::serialize(&inner.project).into_bytes();
        let base_dsl = inner.dsl_source.clone();
        let base_revision = inner.revision;
        inner
            .begin_transaction("bytes".into(), "test".into())
            .unwrap();
        edit_tempo(&mut inner, 90.0);
        edit_tempo(&mut inner, 130.0);
        inner.rollback_transaction().unwrap();
        assert_eq!(oc_dsl::serialize(&inner.project).into_bytes(), base_bytes);
        assert_eq!(inner.dsl_source, base_dsl);
        assert_eq!(inner.revision, base_revision + 1);
        assert!(inner.undo.undo.is_empty());
        assert!(inner.undo.redo.is_empty());
    }

    #[test]
    fn undo_sees_exactly_one_entry_per_transaction() {
        let mut inner = test_inner();
        inner
            .begin_transaction("three edits".into(), "test".into())
            .unwrap();
        edit_tempo(&mut inner, 100.0);
        edit_tempo(&mut inner, 120.0);
        edit_tempo(&mut inner, 140.0);
        // Each edit is its own entry until the batch collapses them.
        assert!(!inner.undo.undo.is_empty());
        inner.commit_transaction().unwrap();
        assert_eq!(inner.undo.undo.len(), 1);
        assert_eq!(
            inner.undo.undo.last().unwrap().label,
            "Transaction: three edits"
        );
        let mut project = inner.project.clone();
        let label = inner.undo.undo(&mut project).expect("one undo entry");
        assert_eq!(label, "Transaction: three edits");
        assert_eq!(project.properties.tempo, 120.0);
        let mut redone = project.clone();
        let label = inner.undo.redo(&mut redone).expect("one redo entry");
        assert_eq!(label, "Redo: Transaction: three edits");
        assert_eq!(redone.properties.tempo, 140.0);
    }

    #[test]
    fn a_no_edit_batch_rolls_back_without_moving_the_revision() {
        let mut inner = test_inner();
        let revision = inner.revision;
        inner
            .begin_transaction("empty".into(), "test".into())
            .unwrap();
        let (_, after) = inner.rollback_transaction().unwrap();
        assert_eq!(after, revision);
    }

    #[test]
    fn commit_squashes_the_batch_into_one_undo_entry() {
        let mut inner = test_inner();
        edit_tempo(&mut inner, 100.0);
        let revision_before_batch = inner.revision;
        inner
            .begin_transaction("batch".into(), "test".into())
            .unwrap();
        edit_tempo(&mut inner, 130.0);
        edit_tempo(&mut inner, 142.0);
        let revision = inner.commit_transaction().unwrap().1;
        assert_eq!(revision, revision_before_batch + 1);
        assert_eq!(inner.project.properties.tempo, 142.0);
        assert_eq!(inner.undo.undo.len(), 2);
        assert_eq!(inner.undo.undo.last().unwrap().label, "Transaction: batch");
        assert!(inner.transaction.is_none());
        assert!(
            inner
                .begin_transaction("again".into(), "test".into())
                .is_ok()
        );
        assert!(
            inner
                .begin_transaction("twice".into(), "test".into())
                .is_err()
        );
        inner.rollback_transaction().unwrap();
        assert!(inner.commit_transaction().is_err());
    }

    #[test]
    fn revision_guard_rejects_stale_and_accepts_the_transaction_base() {
        let mut inner = test_inner();
        assert!(inner.check_revision(None).is_ok());
        assert!(inner.check_revision(Some(0)).is_ok());
        inner.bump_revision();
        let error = inner.check_revision(Some(0)).unwrap_err();
        assert!(error.contains("stale revision 0"), "{error}");
        assert!(inner.check_revision(Some(1)).is_ok());

        inner
            .begin_transaction("batch".into(), "test".into())
            .unwrap();
        inner.bump_revision();
        // The batch owns the bump: the revision stays at the base until commit.
        assert!(inner.check_revision(Some(1)).is_ok());
        assert!(inner.check_revision(Some(2)).is_err());
        assert!(inner.check_revision(Some(0)).is_err());
        inner.rollback_transaction().unwrap();
        assert!(inner.check_revision(Some(1)).is_err());
        assert!(inner.check_revision(Some(2)).is_ok());
    }

    // ── Scoped, gesture-driven undo ────────────────────────────────────────

    use std::time::Duration;

    fn stack_edit(
        stack: &mut UndoStack,
        project: &mut Project,
        name: &str,
        scope: UndoScope,
        tempo: f64,
    ) {
        let old = project.clone();
        project.properties.tempo = tempo;
        project.name = Some(name.to_string());
        stack
            .push(old, project, UndoEdit::new(name, scope))
            .expect("the test edit records");
    }

    #[test]
    fn a_slow_gesture_is_one_entry_regardless_of_elapsed_time() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        stack.begin_gesture(UndoScope::MixerLevels);
        for step in 0..200 {
            let old = project.clone();
            project.properties.tempo = 100.0 + step as f64 * 0.01;
            stack
                .push_at(
                    old,
                    &project,
                    UndoEdit::new("mixer levels", UndoScope::MixerLevels),
                    start + Duration::from_millis(step * 10),
                )
                .expect("the test edit records");
        }
        assert!(stack.is_in_gesture());
        stack.end_gesture();
        assert!(!stack.is_in_gesture());

        assert_eq!(stack.undo.len(), 1, "2 s of updates is still one entry");
        let history = stack.history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].name, "mixer levels");
        assert_eq!(history[0].scopes, UndoScopes::one(UndoScope::MixerLevels));
        assert_eq!(history[0].timestamp, 1_700_000_000_000);
        assert!(history[0].applied);
    }

    #[test]
    fn unrelated_quick_edits_stay_separate() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack_edit(
            &mut stack,
            &mut project,
            "tempo",
            UndoScope::Transport,
            121.0,
        );
        stack_edit(
            &mut stack,
            &mut project,
            "clip move",
            UndoScope::Project,
            122.0,
        );
        assert_eq!(stack.undo.len(), 2, "different scopes never merge");
        assert_eq!(stack.history()[1].name, "clip move");
    }

    #[test]
    fn same_scope_edits_inside_a_gesture_merge() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack.begin_gesture(UndoScope::MixerLevels);
        stack_edit(
            &mut stack,
            &mut project,
            "mixer levels",
            UndoScope::MixerLevels,
            123.0,
        );
        stack_edit(
            &mut stack,
            &mut project,
            "mixer levels",
            UndoScope::MixerLevels,
            124.0,
        );
        assert_eq!(stack.undo.len(), 1, "the gesture merged both updates");
        stack.end_gesture();

        stack_edit(
            &mut stack,
            &mut project,
            "mixer levels",
            UndoScope::MixerLevels,
            125.0,
        );
        assert_eq!(stack.undo.len(), 2, "the gesture ended, so this is new");
    }

    #[test]
    fn one_gesture_merges_every_later_push_into_the_entry_it_opened() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        let start_tempo = project.properties.tempo;
        stack.begin_gesture(UndoScope::Project);
        for (step, tempo) in [121.0, 122.0, 123.0].into_iter().enumerate() {
            let name = format!("clip move {step}");
            stack_edit(&mut stack, &mut project, &name, UndoScope::Project, tempo);
        }
        stack.end_gesture();

        assert_eq!(stack.undo.len(), 1, "three pushes are one drag");
        let mut reverted = project.clone();
        stack.undo(&mut reverted).unwrap();
        assert_eq!(
            reverted.properties.tempo, start_tempo,
            "undo reverts all three pushes"
        );
    }

    #[test]
    fn two_gestures_are_two_entries_and_undo_reverts_only_the_second() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack.begin_gesture(UndoScope::Project);
        stack_edit(
            &mut stack,
            &mut project,
            "move clip 1",
            UndoScope::Project,
            121.0,
        );
        stack.end_gesture();

        stack.begin_gesture(UndoScope::Project);
        stack_edit(
            &mut stack,
            &mut project,
            "move clip 2",
            UndoScope::Project,
            122.0,
        );
        stack.end_gesture();

        let labels: Vec<String> = stack.history().into_iter().map(|e| e.name).collect();
        assert_eq!(labels, ["move clip 1", "move clip 2"]);

        let mut reverted = project.clone();
        assert_eq!(stack.undo(&mut reverted).unwrap(), "move clip 2");
        assert_eq!(
            reverted.properties.tempo, 121.0,
            "only the second drag reverted"
        );
    }

    #[test]
    fn a_gesture_after_a_plain_edit_does_not_extend_its_entry() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack_edit(
            &mut stack,
            &mut project,
            "add clip",
            UndoScope::Project,
            121.0,
        );

        stack.begin_gesture(UndoScope::Project);
        stack_edit(
            &mut stack,
            &mut project,
            "clip move",
            UndoScope::Project,
            122.0,
        );
        stack.end_gesture();

        assert_eq!(stack.undo.len(), 2, "the drag opened its own entry");
        let labels: Vec<String> = stack.history().into_iter().map(|e| e.name).collect();
        assert_eq!(labels, ["add clip", "clip move"]);

        let mut reverted = project.clone();
        assert_eq!(stack.undo(&mut reverted).unwrap(), "clip move");
        assert_eq!(
            reverted.properties.tempo, 121.0,
            "undo reverts the drag, not the plain edit"
        );
    }

    #[test]
    fn nested_gestures_share_the_outer_entry() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        let start_tempo = project.properties.tempo;
        stack.begin_gesture(UndoScope::Project);
        stack.begin_gesture(UndoScope::MixerLevels);
        stack_edit(
            &mut stack,
            &mut project,
            "clip move",
            UndoScope::Project,
            121.0,
        );
        stack.end_gesture();
        assert!(stack.is_in_gesture(), "the outer gesture is still open");
        stack_edit(
            &mut stack,
            &mut project,
            "clip move",
            UndoScope::Project,
            122.0,
        );
        stack.end_gesture();
        assert!(!stack.is_in_gesture());

        assert_eq!(stack.undo.len(), 1, "a nested gesture is still one entry");
        let mut reverted = project.clone();
        stack.undo(&mut reverted).unwrap();
        assert_eq!(reverted.properties.tempo, start_tempo);
    }

    #[test]
    fn a_gesture_that_returns_to_its_start_leaves_no_entry() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        let before = project.clone();
        stack.begin_gesture(UndoScope::Project);
        stack_edit(
            &mut stack,
            &mut project,
            "clip move",
            UndoScope::Project,
            121.0,
        );
        assert_eq!(stack.undo.len(), 1);

        let old = std::mem::replace(&mut project, before.clone());
        stack
            .push(
                old,
                &project,
                UndoEdit::new("clip move", UndoScope::Project),
            )
            .expect("the test edit records");
        stack.end_gesture();
        assert!(stack.undo.is_empty(), "no net change leaves no entry");
    }

    #[test]
    fn a_gesture_that_changes_nothing_keeps_the_previous_entry() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack_edit(
            &mut stack,
            &mut project,
            "add clip",
            UndoScope::Project,
            121.0,
        );
        stack.begin_gesture(UndoScope::Project);
        let old = project.clone();
        stack
            .push(
                old,
                &project,
                UndoEdit::new("clip move", UndoScope::Project),
            )
            .expect("the test edit records");
        stack.end_gesture();

        assert_eq!(stack.undo.len(), 1, "the empty gesture added nothing");
        assert_eq!(stack.undo[0].label, "add clip", "and removed nothing");
    }

    #[test]
    fn explicit_merge_needs_an_intersecting_scope() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack_edit(
            &mut stack,
            &mut project,
            "note move",
            UndoScope::Project,
            126.0,
        );
        let old = project.clone();
        project.properties.tempo = 127.0;
        stack
            .push(
                old,
                &project,
                UndoEdit::new("note move", UndoScope::Project).merge(),
            )
            .expect("the test edit records");
        assert_eq!(stack.undo.len(), 1, "same scope plus merge appends");

        let old = project.clone();
        project.properties.tempo = 128.0;
        stack
            .push(
                old,
                &project,
                UndoEdit::new("tempo", UndoScope::Transport).merge(),
            )
            .expect("the test edit records");
        assert_eq!(stack.undo.len(), 2, "no scope intersection, no merge");
    }

    #[test]
    fn restoring_a_level_matches_replaying_the_edits_forward() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        let mut forward = vec![oc_dsl::serialize(&project).into_bytes()];
        for (name, scope, tempo) in [
            ("note move", UndoScope::Project, 121.0),
            ("mixer levels", UndoScope::MixerLevels, 122.0),
            ("tempo", UndoScope::Transport, 123.0),
            ("clip move", UndoScope::Project, 124.0),
        ] {
            let old = project.clone();
            project.properties.tempo = tempo;
            stack
                .push(old, &project, UndoEdit::new(name, scope))
                .expect("the test edit records");
            forward.push(oc_dsl::serialize(&project).into_bytes());
        }
        assert_eq!(stack.undo.len(), 4);

        let mut jumped = project.clone();
        stack.restore_undo_level(2, &mut jumped).unwrap();
        assert_eq!(
            oc_dsl::serialize(&jumped).into_bytes(),
            forward[2],
            "level 2 must be byte-identical to replaying two edits"
        );
        let history = stack.history();
        assert_eq!(history.len(), 4);
        assert!(history[0].applied && history[1].applied);
        assert!(!history[2].applied && !history[3].applied);
        assert_eq!(history[1].index, 2);

        stack.restore_undo_level(4, &mut jumped).unwrap();
        assert_eq!(oc_dsl::serialize(&jumped).into_bytes(), forward[4]);
        assert!(stack.history().iter().all(|entry| entry.applied));

        stack.restore_undo_level(0, &mut jumped).unwrap();
        assert_eq!(oc_dsl::serialize(&jumped).into_bytes(), forward[0]);
        assert!(stack.restore_undo_level(5, &mut jumped).is_err());
    }

    #[test]
    fn undo_inside_a_batch_rewinds_its_last_step_and_the_turn_keeps_one_entry() {
        let mut inner = test_inner();
        edit_tempo(&mut inner, 100.0);
        let history_before = inner.undo.undo.len();
        inner
            .begin_transaction("turn".into(), "test".into())
            .unwrap();
        edit_tempo(&mut inner, 130.0);
        edit_tempo(&mut inner, 142.0);

        let label = inner
            .undo_step()
            .expect("a batch must be able to undo its own last step");
        assert_eq!(label, "tempo");
        assert_eq!(inner.project.properties.tempo, 130.0);
        assert!(inner.transaction.is_some(), "the batch keeps going");

        inner.commit_transaction().unwrap();
        assert_eq!(
            inner.undo.undo.len(),
            history_before + 1,
            "the turn publishes exactly one entry"
        );
        assert_eq!(inner.undo.undo.last().unwrap().label, "Transaction: turn");
        assert_eq!(inner.undo_step().unwrap(), "Transaction: turn");
        assert_eq!(
            inner.project.properties.tempo, 100.0,
            "the turn's entry undoes the two edits it survived with"
        );
    }

    #[test]
    fn a_batch_that_undoes_a_pre_turn_edit_rolls_back_to_both_snapshots() {
        let mut inner = test_inner();
        edit_tempo(&mut inner, 100.0);
        let base_project = inner.project.clone();
        inner
            .begin_transaction("undo first".into(), "test".into())
            .unwrap();

        // The turn's first call is undo, so the newest entry predates the batch.
        assert_eq!(inner.undo_step().unwrap(), "tempo");
        assert_ne!(inner.project, base_project);

        inner.rollback_transaction().unwrap();
        assert_eq!(inner.project, base_project, "rollback restores the project");
        assert_eq!(inner.undo.undo.len(), 1, "and the history it found");
        assert!(inner.undo.redo.is_empty());
        assert_eq!(inner.undo.undo.last().unwrap().label, "tempo");
    }

    #[test]
    fn a_new_edit_after_a_jump_drops_the_redo_levels() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack_edit(
            &mut stack,
            &mut project,
            "note move",
            UndoScope::Project,
            131.0,
        );
        stack_edit(
            &mut stack,
            &mut project,
            "clip move",
            UndoScope::Project,
            132.0,
        );
        stack.restore_undo_level(1, &mut project).unwrap();
        assert_eq!(stack.history().len(), 2);

        stack_edit(
            &mut stack,
            &mut project,
            "clip move",
            UndoScope::Project,
            133.0,
        );
        assert_eq!(stack.history().len(), 2, "redo levels were truncated");
        assert!(stack.history().iter().all(|entry| entry.applied));
        assert!(stack.redo.is_empty());
    }

    #[test]
    fn a_corrupt_inverse_leaves_the_project_and_the_history_alone() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack_edit(
            &mut stack,
            &mut project,
            "move clip",
            UndoScope::Project,
            130.0,
        );
        stack.undo.last_mut().unwrap().inverse = b"not a patch".to_vec();

        let mut reverted = project.clone();
        let error = stack.undo(&mut reverted).unwrap_err();
        assert!(error.contains("could not undo"), "{error}");
        assert_eq!(reverted, project, "the project did not change");
        assert_eq!(stack.undo.len(), 1, "the corrupt entry stays on the stack");
        assert!(stack.redo.is_empty(), "nothing moved to the redo stack");
    }

    #[test]
    fn an_encode_failure_is_reported_and_keeps_the_recorded_history() {
        let mut stack = UndoStack::new();
        let mut project = default_project();
        stack_edit(
            &mut stack,
            &mut project,
            "move clip",
            UndoScope::Project,
            130.0,
        );
        let mut reverted = project.clone();
        stack.undo(&mut reverted).unwrap();
        assert_eq!(stack.redo.len(), 1);

        stack.fail_encode = true;
        let old = reverted.clone();
        reverted.properties.tempo = 140.0;
        let error = stack
            .push(old, &reverted, UndoEdit::new("tempo", UndoScope::Transport))
            .unwrap_err();
        assert!(error.contains("encoding"), "{error}");
        assert!(stack.undo.is_empty(), "a failed encode records no entry");
        assert_eq!(stack.redo.len(), 1, "and keeps the redo history");
    }

    // ── Parameter inventory cache ─────────────────────────────────────────

    fn cached_param(name: &str) -> PluginParam {
        PluginParam {
            id: 1,
            name: name.to_string(),
            module: "VST3".to_string(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            current: 0.0,
            display: "0.0000".to_string(),
            flags: 0,
        }
    }

    /// The poll the plugin panel runs: cached when it can, enumerated only on
    /// a miss.
    fn poll_params(
        cache: &mut ParamCache,
        preset: &str,
        enumerate: &mut impl FnMut() -> Vec<PluginParam>,
    ) -> Vec<PluginParam> {
        if let Some(cached) = cache.get(preset) {
            return cached.to_vec();
        }
        let params = enumerate();
        cache.store(preset, params.clone());
        params
    }

    #[test]
    fn a_param_poll_enumerates_once_per_second_until_the_plugin_reports_a_change() {
        let mut cache = ParamCache::default();
        let enumerations = std::cell::Cell::new(0);
        let mut enumerate = || {
            enumerations.set(enumerations.get() + 1);
            vec![cached_param("Gain")]
        };

        // Four polls in one second, a fast panel poll rate.
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            assert_eq!(poll_params(&mut cache, "Piano", &mut enumerate).len(), 1);
            std::thread::sleep(Duration::from_millis(250));
        }
        assert_eq!(
            enumerations.get(),
            1,
            "a poll with no reported change reuses the inventory"
        );

        // The plugin reported a change: the next poll reads it again.
        cache.invalidate("Piano");
        assert_eq!(poll_params(&mut cache, "Piano", &mut enumerate).len(), 1);
        assert_eq!(enumerations.get(), 2, "a change costs one enumeration");
    }
}

#[cfg(test)]
#[path = "state/undo_failure_tests.rs"]
mod undo_failure_tests;
