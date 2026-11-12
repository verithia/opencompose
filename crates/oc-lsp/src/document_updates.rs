//! Document admission and publication belong to one incarnation/version owner.
//! Closed documents leave no tombstones; outstanding work retains only its lease.
use super::*;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DocumentVersion(pub i32);
#[derive(Clone, Copy)]
pub(crate) enum Admission {
    Open(DocumentVersion),
    Change(DocumentVersion),
}
struct Slot {
    incarnation: Arc<()>,
    update: Arc<()>,
    version: DocumentVersion,
    needs_clear: bool,
}
#[derive(Default)]
pub(crate) struct Documents {
    pub cache: HashMap<String, DocumentState>,
    slots: HashMap<String, Slot>,
}
#[derive(Clone)]
pub(crate) struct UpdateLease {
    uri: String,
    incarnation: Arc<()>,
    update: Arc<()>,
    version: DocumentVersion,
}
impl Documents {
    /// Admit an open or changed document and return the lease that owns its
    /// published state. A change at or below the stored version is refused, and
    /// the cached diagnostics are dropped so no request serves the old text.
    pub fn admit(&mut self, uri: &str, admission: Admission) -> Option<UpdateLease> {
        let slot = match admission {
            Admission::Open(version) => {
                self.slots.insert(
                    uri.to_owned(),
                    Slot {
                        incarnation: Arc::new(()),
                        update: Arc::new(()),
                        version,
                        needs_clear: true,
                    },
                );
                self.slots.get_mut(uri)?
            }
            Admission::Change(version) => {
                let slot = self.slots.get_mut(uri)?;
                if version <= slot.version {
                    return None;
                }
                slot.version = version;
                slot.update = Arc::new(());
                slot
            }
        };
        let lease = UpdateLease {
            uri: uri.to_owned(),
            incarnation: slot.incarnation.clone(),
            update: slot.update.clone(),
            version: slot.version,
        };
        // Hover/completion must not serve the previous text while this admitted
        // change is analyzed or rejected by the source-size owner.
        self.cache.remove(uri);
        Some(lease)
    }
    fn current(&self, lease: &UpdateLease) -> bool {
        self.slots.get(&lease.uri).is_some_and(|slot| {
            Arc::ptr_eq(&slot.incarnation, &lease.incarnation)
                && Arc::ptr_eq(&slot.update, &lease.update)
                && slot.version == lease.version
        })
    }
    /// Forget the document's incarnation, version and cached diagnostics.
    pub fn close(&mut self, uri: &str) {
        self.slots.remove(uri);
        self.cache.remove(uri);
    }
    #[cfg(test)]
    pub fn next_test_admission(&self, uri: &str) -> Option<Admission> {
        match self.slots.get(uri) {
            Some(slot) => slot
                .version
                .0
                .checked_add(1)
                .map(|version| Admission::Change(DocumentVersion(version))),
            None => Some(Admission::Open(DocumentVersion(0))),
        }
    }
}

#[tower_lsp::async_trait]
pub(crate) trait Publisher: std::marker::Send + Sync {
    async fn publish(&self, uri: Url, diagnostics: Vec<Diagnostic>, version: Option<i32>);
}
#[tower_lsp::async_trait]
impl Publisher for Client {
    async fn publish(&self, uri: Url, diagnostics: Vec<Diagnostic>, version: Option<i32>) {
        self.publish_diagnostics(uri, diagnostics, version).await;
    }
}

impl Backend {
    pub(crate) fn admit_document(&self, uri: &str, admission: Admission) -> Option<UpdateLease> {
        lock_documents(&self.documents).admit(uri, admission)
    }
    pub(crate) fn analyze_document(
        &self,
        lease: &UpdateLease,
        source: &str,
        limits: oc_dsl::source_map::SourceLimits,
    ) -> Option<Vec<Diagnostic>> {
        let diagnostics::DocumentAnalysis { cache, errors } =
            diagnostics::analyze_with_limits(source, limits);
        let protocol = errors.iter().map(protocol_diagnostic).collect();
        let mut documents = lock_documents(&self.documents);
        if !documents.current(lease) {
            return None;
        }
        match cache {
            diagnostics::CacheAdmission::Admitted { source, project } => {
                documents.cache.insert(
                    lease.uri.clone(),
                    DocumentState {
                        source: source.to_owned(),
                        project: project.map(|project| *project),
                        errors,
                    },
                );
            }
            diagnostics::CacheAdmission::Rejected => {
                documents.cache.remove(&lease.uri);
            }
        }
        Some(protocol)
    }
    pub(crate) async fn publish_document(
        &self,
        lease: UpdateLease,
        diagnostics: Vec<Diagnostic>,
        publisher: &impl Publisher,
    ) {
        let Ok(uri) = lease.uri.parse::<Url>() else {
            return;
        };
        // Network serialization is separate from the registry lock. New edits
        // and closes can revoke ownership while a notification is awaiting IO.
        let _publication = self.publication.lock().await;
        let clear = {
            let documents = lock_documents(&self.documents);
            if !documents.current(&lease) {
                return;
            }
            documents.slots[&lease.uri].needs_clear
        };
        if clear {
            publisher.publish(uri.clone(), Vec::new(), None).await;
            let mut documents = lock_documents(&self.documents);
            if let Some(slot) = documents.slots.get_mut(&lease.uri)
                && Arc::ptr_eq(&slot.incarnation, &lease.incarnation)
            {
                slot.needs_clear = false;
            }
        }
        if !lock_documents(&self.documents).current(&lease) {
            return;
        }
        publisher
            .publish(uri, diagnostics, Some(lease.version.0))
            .await;
        // No cache mutation follows publication: a delayed sender cannot
        // recreate a closed document or replace a newer committed snapshot.
    }
    pub(crate) async fn publish_closed(&self, uri: &str, publisher: &impl Publisher) {
        let Ok(parsed) = uri.parse::<Url>() else {
            return;
        };
        let _publication = self.publication.lock().await;
        if lock_documents(&self.documents).slots.contains_key(uri) {
            return;
        }
        publisher.publish(parsed, Vec::new(), None).await;
    }
    pub(crate) async fn run_update(
        &self,
        lease: UpdateLease,
        source: &str,
        limits: oc_dsl::source_map::SourceLimits,
        publisher: &impl Publisher,
    ) {
        if let Some(diagnostics) = self.analyze_document(&lease, source, limits) {
            self.publish_document(lease, diagnostics, publisher).await;
        }
    }
}

#[cfg(test)]
mod tests;
