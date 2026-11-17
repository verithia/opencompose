//! Test-only entry points on the session store. They stage state the
//! production commands would otherwise only reach through the UI, but every
//! assertion they enable runs the production read, write and compaction paths.

use super::*;

impl Store {
    pub(in crate::ai) fn with_lookup(
        root: impl Into<PathBuf>,
        lookup: Arc<dyn lookup::Io>,
    ) -> Self {
        Self {
            root: root.into(),
            lookup,
            session_cap: MAX_SESSION_BYTES,
        }
    }

    pub(in crate::ai) fn with_session_cap(mut self, cap: u64) -> Self {
        self.session_cap = cap;
        self
    }

    pub fn reassociate(&self, from: &Path, to: &Path) -> Result<String, String> {
        let _lock = self.mutation_lock()?;
        let from_key = hashed_key(from);
        let source = self.root.join(&from_key);
        if !source.is_dir() {
            return Err(format!("no saved conversations for {}", from.display()));
        }
        let to_key = hashed_key(to);
        let target = self.root.join(&to_key);
        if target.exists() {
            return Err(format!("conversations already exist for {}", to.display()));
        }
        private_fs::ensure_dir(&self.root).map_err(|error| format!("sessions: {error}"))?;
        std::fs::rename(&source, &target).map_err(|error| format!("sessions: {error}"))?;
        self.ensure_workspace(&to_key, Some(to), &now())?;
        Ok(to_key)
    }

    pub fn record_history_mark(
        &self,
        id: &str,
        project: &Path,
        mark: HistoryMark,
    ) -> Result<(), String> {
        let _lock = self.mutation_lock()?;
        let (dir, mut meta) = self.find(id)?;
        let project = BoundProject::of(project);
        if meta.project_path.as_ref() != Some(&project) {
            return Ok(());
        }
        let saved = SavedHistoryMark::for_project(project, mark.at, mark.max_id);
        if meta.history_mark.as_ref() == Some(&saved) {
            return Ok(());
        }
        meta.history_mark = Some(saved);
        write_meta(&dir, &meta)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        self.delete_guarded(id, &|| Ok(()))
            .map_err(|error| error.message)
    }

    pub fn drop_history(&self, id: &str, keep: u64) -> Result<Value, String> {
        self.drop_history_guarded(id, keep, &|| Ok(()))
            .map_err(|error| error.message)
    }

    pub fn drop_history_guarded(
        &self,
        id: &str,
        keep: u64,
        guard: Guard<'_>,
    ) -> Result<Value, SessionError> {
        let _lock = self.mutation_lock()?;
        guard()?;
        if keep == 0 {
            return Err("drop_history needs a count of at least 1".to_owned().into());
        }
        let (dir, mut meta) = self.find(id)?;
        let records = read_records(&dir)?;
        let count = compaction_indices(&records).len();
        let keep = keep as usize;
        let Some((from, kept)) = keep_from_compaction(&records, keep) else {
            return Err(
                format!("drop_history {keep}: this session has {count} compaction(s)").into(),
            );
        };
        let dropped = records.len() - from;
        write_records(&dir, &kept)?;
        meta.updated = now();
        write_meta(&dir, &meta)?;
        let removed = self.collect_unreferenced_locked();
        Ok(json!({
            "dropped_records": dropped,
            "kept_records": kept.len(),
            "removed_assets": removed,
        }))
    }

    pub fn collect_unreferenced(&self) -> Result<u64, String> {
        let _lock = self.mutation_lock()?;
        Ok(self.collect_unreferenced_locked())
    }

    pub fn prepare(
        &self,
        id: &str,
        incoming: &[Value],
        project: Option<&Path>,
        context: PrepareContext<'_>,
    ) -> Result<Prepared, String> {
        self.prepare_owned(id, incoming, project, context, None)
    }

    pub fn prepare_owned(
        &self,
        id: &str,
        incoming: &[Value],
        project: Option<&Path>,
        context: PrepareContext<'_>,
        incarnation: Option<&str>,
    ) -> Result<Prepared, String> {
        let mut prepared =
            match self.prepare_admitted(id, incoming, project, context, incarnation, |_| {
                Ok::<(), String>(())
            }) {
                Ok(prepared) => prepared,
                Err(PrepareFailure::Binding(mismatch)) => return Err(mismatch.message()),
                Err(PrepareFailure::Admitted(message)) => return Err(message),
                Err(PrepareFailure::Store(message)) => return Err(message),
            };
        // The mechanical path commits the plan it just built. A real turn keeps
        // the plan, asks the model outside the lock and commits the answer.
        if let Some(plan) = prepared.pending_compaction.take() {
            let resolution = self.finish_compaction(plan, None);
            if let Some(compaction) = resolution.compaction.as_ref() {
                prepared.apply(compaction);
            }
            prepared.compaction_error = resolution.failure;
        }
        Ok(prepared)
    }

    pub fn record_reply(
        &self,
        id: &str,
        reply: &Value,
        facts: &ProjectFacts,
        provider: &str,
        model: &str,
    ) -> Result<SessionMeta, String> {
        self.record_reply_owned(id, reply, facts, (provider, model), None)
    }

    pub fn compact(
        &self,
        id: &str,
        target_tokens: Option<u64>,
        facts: &ProjectFacts,
    ) -> Result<Compaction, String> {
        let _lock = self.mutation_lock()?;
        self.compact_locked(id, target_tokens, facts)
    }
}

pub(crate) struct TestRootGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

pub(crate) fn test_root(root: PathBuf) -> TestRootGuard {
    let lock = TEST_ROOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut slot) = TEST_ROOT.lock() {
        *slot = Some(root);
    }
    TestRootGuard { _lock: lock }
}
