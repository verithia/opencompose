use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

type Publication = (Option<i32>, Vec<String>);
struct GatedPublisher {
    calls: AtomicUsize,
    blocked: usize,
    entered: Semaphore,
    release: Semaphore,
    delivered: Mutex<Vec<Publication>>,
}
impl GatedPublisher {
    fn new(blocked: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            blocked,
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            delivered: Mutex::new(Vec::new()),
        }
    }
    async fn wait(&self) {
        self.entered.acquire().await.unwrap().forget();
    }
    fn resume(&self) {
        self.release.add_permits(1);
    }
    fn delivered(&self) -> Vec<Publication> {
        self.delivered.lock().unwrap().clone()
    }
}
#[tower_lsp::async_trait]
impl Publisher for GatedPublisher {
    async fn publish(&self, _: Url, diagnostics: Vec<Diagnostic>, version: Option<i32>) {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == self.blocked {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        self.delivered.lock().unwrap().push((
            version,
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.message)
                .collect(),
        ));
    }
}
async fn together(
    first: impl std::future::Future<Output = ()>,
    second: impl std::future::Future<Output = ()>,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(first, second);
    })
    .await
    .expect("controlled publication barriers must finish");
}
const URI: &str = "file:///owner.ocp";
const OLD: &str = "@project\n  tempo: 0\n";
const NEW: &str = "@project\n  tempo: 120\n";
fn open(backend: &Backend, version: i32) -> UpdateLease {
    backend
        .admit_document(URI, Admission::Open(DocumentVersion(version)))
        .unwrap()
}
fn change(backend: &Backend, version: i32) -> UpdateLease {
    backend
        .admit_document(URI, Admission::Change(DocumentVersion(version)))
        .unwrap()
}
fn analyze(backend: &Backend, lease: &UpdateLease, text: &str) -> Vec<Diagnostic> {
    backend
        .analyze_document(lease, text, Default::default())
        .unwrap()
}

#[tokio::test]
async fn document_owner_newer_edit_while_old_publication_waits_keeps_final_cache_and_notification()
{
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let publisher = GatedPublisher::new(1);
    let old = open(backend, 1);
    let diagnostics = analyze(backend, &old, OLD);
    together(
        backend.publish_document(old, diagnostics, &publisher),
        async {
            publisher.wait().await;
            let newer = change(backend, 2);
            assert!(!lock_documents(&backend.documents).cache.contains_key(URI));
            let diagnostics = analyze(backend, &newer, NEW);
            assert_eq!(lock_documents(&backend.documents).cache[URI].source, NEW);
            publisher.resume();
            backend
                .publish_document(newer, diagnostics, &publisher)
                .await;
        },
    )
    .await;
    let delivered = publisher.delivered();
    assert_eq!(
        delivered.iter().map(|item| item.0).collect::<Vec<_>>(),
        [None, Some(1), Some(2)]
    );
    assert!(!delivered[1].1.is_empty());
    assert!(delivered[2].1.is_empty());
    assert_eq!(lock_documents(&backend.documents).cache[URI].source, NEW);
}

#[tokio::test]
async fn document_owner_close_during_publication_clears_last_and_never_recreates_cache() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let publisher = GatedPublisher::new(1);
    let old = open(backend, 1);
    let diagnostics = analyze(backend, &old, OLD);
    together(
        backend.publish_document(old, diagnostics, &publisher),
        async {
            publisher.wait().await;
            lock_documents(&backend.documents).close(URI);
            assert!(!lock_documents(&backend.documents).cache.contains_key(URI));
            publisher.resume();
            backend.publish_closed(URI, &publisher).await;
        },
    )
    .await;
    assert!(lock_documents(&backend.documents).cache.is_empty());
    assert!(
        backend
            .admit_document(URI, Admission::Change(DocumentVersion(2)))
            .is_none()
    );
    let delivered = publisher.delivered();
    assert_eq!(
        delivered.iter().map(|item| item.0).collect::<Vec<_>>(),
        [None, Some(1), None]
    );
    assert!(delivered.last().unwrap().1.is_empty());
}

#[tokio::test]
async fn document_owner_same_uri_same_version_reopen_resets_after_blocked_old_publication() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let publisher = GatedPublisher::new(1);
    let old = open(backend, 7);
    let diagnostics = analyze(backend, &old, OLD);
    together(
        backend.publish_document(old, diagnostics, &publisher),
        async {
            publisher.wait().await;
            lock_documents(&backend.documents).close(URI);
            let reopened = open(backend, 7);
            let diagnostics = analyze(backend, &reopened, NEW);
            publisher.resume();
            backend.publish_closed(URI, &publisher).await;
            backend
                .publish_document(reopened, diagnostics, &publisher)
                .await;
        },
    )
    .await;
    let delivered = publisher.delivered();
    assert_eq!(
        delivered.iter().map(|item| item.0).collect::<Vec<_>>(),
        [None, Some(7), None, Some(7)]
    );
    assert!(!delivered[1].1.is_empty());
    assert!(delivered.last().unwrap().1.is_empty());
    assert_eq!(lock_documents(&backend.documents).cache[URI].source, NEW);
}

#[tokio::test]
async fn document_owner_new_change_during_reopen_clear_supersedes_reopen_payload() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let publisher = GatedPublisher::new(2);
    let initial = open(backend, 3);
    backend
        .run_update(initial, OLD, Default::default(), &publisher)
        .await;
    lock_documents(&backend.documents).close(URI);
    let reopened = open(backend, 3);
    let diagnostics = analyze(backend, &reopened, OLD);
    together(
        backend.publish_document(reopened, diagnostics, &publisher),
        async {
            publisher.wait().await;
            let newer = change(backend, 4);
            let diagnostics = analyze(backend, &newer, NEW);
            publisher.resume();
            backend
                .publish_document(newer, diagnostics, &publisher)
                .await;
        },
    )
    .await;
    let delivered = publisher.delivered();
    assert_eq!(
        delivered.iter().map(|item| item.0).collect::<Vec<_>>(),
        [None, Some(3), None, Some(4)]
    );
    assert!(delivered.last().unwrap().1.is_empty());
    assert_eq!(lock_documents(&backend.documents).cache[URI].source, NEW);
}

#[tokio::test]
async fn document_owner_reversed_analysis_and_late_old_change_cannot_publish_or_commit() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let publisher = GatedPublisher::new(usize::MAX);
    let old = open(backend, 1);
    let newer = change(backend, 2);
    let diagnostics = analyze(backend, &newer, NEW);
    backend
        .publish_document(newer, diagnostics, &publisher)
        .await;
    assert!(
        backend
            .analyze_document(&old, OLD, Default::default())
            .is_none()
    );
    backend.publish_document(old, Vec::new(), &publisher).await;
    assert_eq!(
        publisher.delivered(),
        [(None, Vec::new()), (Some(2), Vec::new())]
    );
    assert_eq!(lock_documents(&backend.documents).cache[URI].source, NEW);
}

#[tokio::test]
async fn document_owner_close_during_analysis_and_reopen_preserves_new_incarnation() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let publisher = GatedPublisher::new(usize::MAX);
    let old = open(backend, 5);
    lock_documents(&backend.documents).close(URI);
    let reopened = open(backend, 5);
    assert!(
        backend
            .analyze_document(&old, OLD, Default::default())
            .is_none()
    );
    backend
        .run_update(reopened, NEW, Default::default(), &publisher)
        .await;
    assert_eq!(
        publisher.delivered(),
        [(None, Vec::new()), (Some(5), Vec::new())]
    );
    assert_eq!(lock_documents(&backend.documents).cache[URI].source, NEW);
}

#[test]
fn document_owner_versions_are_strictly_monotonic_within_each_incarnation() {
    let mut documents = Documents::default();
    assert!(
        documents
            .admit(URI, Admission::Change(DocumentVersion(1)))
            .is_none()
    );
    documents
        .admit(URI, Admission::Open(DocumentVersion(i32::MAX)))
        .unwrap();
    assert!(
        documents
            .admit(URI, Admission::Change(DocumentVersion(i32::MAX)))
            .is_none()
    );
    assert!(
        documents
            .admit(URI, Admission::Change(DocumentVersion(i32::MIN)))
            .is_none()
    );
    documents.close(URI);
    assert!(documents.slots.is_empty());
    assert!(documents.cache.is_empty());
    assert!(
        documents
            .admit(URI, Admission::Open(DocumentVersion(i32::MIN)))
            .is_some()
    );
}

#[tokio::test]
async fn document_owner_rejected_newer_text_keeps_version_admission_without_stale_ast() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let publisher = GatedPublisher::new(usize::MAX);
    let old = open(backend, 1);
    analyze(backend, &old, NEW);
    let newer = change(backend, 2);
    let limits = oc_dsl::source_map::SourceLimits {
        document: oc_dsl::diff::compressed::CodecLimits {
            max_encoded_bytes: 32,
            max_decoded_bytes: 32,
        },
        ..Default::default()
    };
    let text = format!("@project \"{}\"\n", "x".repeat(40));
    backend.run_update(newer, &text, limits, &publisher).await;
    assert!(lock_documents(&backend.documents).cache.is_empty());
    assert!(
        backend
            .analyze_document(&old, NEW, Default::default())
            .is_none()
    );
    assert!(
        backend
            .admit_document(URI, Admission::Change(DocumentVersion(2)))
            .is_none()
    );
    let delivered = publisher.delivered();
    assert_eq!(
        delivered.iter().map(|item| item.0).collect::<Vec<_>>(),
        [None, Some(2)]
    );
    assert_eq!(delivered[1].1.len(), 1);
    let recovered = change(backend, 3);
    backend
        .run_update(recovered, NEW, Default::default(), &publisher)
        .await;
    assert_eq!(lock_documents(&backend.documents).cache[URI].source, NEW);
    assert_eq!(
        publisher.delivered().last().unwrap(),
        &(Some(3), Vec::new())
    );
}
