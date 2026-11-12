//! OpenCompose LSP server — provides diagnostics, completion, and hover
//! for .ocp files. Used by VS Code and as the validation layer for AI tools.

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use oc_dsl::ast::*;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

mod completion;
mod diagnostics;
mod document_updates;
use document_updates::{Admission, DocumentVersion, Documents};
mod hover;
mod position;
#[cfg(test)]
mod tests;
mod validate;

pub use validate::validate_project;

struct Backend {
    client: Client,
    /// Cached AST per document URI.
    documents: Mutex<Documents>,
    publication: tokio::sync::Mutex<()>,
}

struct DocumentState {
    source: String,
    project: Option<Project>,
    #[allow(dead_code)]
    errors: Vec<DiagnosticInfo>,
}

struct DiagnosticInfo {
    range: Range,
    message: String,
    severity: DiagnosticSeverity,
    location: diagnostics::LocationStatus,
}

fn protocol_diagnostic(info: &DiagnosticInfo) -> Diagnostic {
    Diagnostic {
        range: info.range,
        severity: Some(info.severity),
        source: Some("opencompose".into()),
        message: info.message.clone(),
        // LSP requires a range even when a check has no source subject. Keep
        // that transport anchor distinct from an actual document constraint.
        data: Some(serde_json::json!({ "source_location": info.location })),
        ..Default::default()
    }
}

fn lock_documents<T>(documents: &Mutex<T>) -> MutexGuard<'_, T> {
    documents.lock().unwrap_or_else(|poisoned| {
        // Cached source/AST tuples are replaced as complete snapshots. Reader
        // callbacks do not mutate registry admission or snapshot fields, so an
        // interrupted reader leaves ownership and cached tuples intact.
        let guard = poisoned.into_inner();
        documents.clear_poison();
        guard
    })
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec!["@".into(), ":".into(), "=".into(), " ".into()]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: server_capabilities(),
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "OpenCompose LSP initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let source = params.text_document.text;
        if let Some(lease) = self.admit_document(
            &uri,
            Admission::Open(DocumentVersion(params.text_document.version)),
        ) {
            self.run_update(lease, &source, Default::default(), &self.client)
                .await;
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        if let Some(change) = params.content_changes.into_iter().last()
            && let Some(lease) = self.admit_document(
                &uri,
                Admission::Change(DocumentVersion(params.text_document.version)),
            )
        {
            self.run_update(lease, &change.text, Default::default(), &self.client)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        lock_documents(&self.documents).close(&uri);
        self.publish_closed(&uri, &self.client).await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri.to_string();
        let pos = params.text_document_position.position;
        let docs = lock_documents(&self.documents);

        if let Some(doc) = docs.cache.get(&uri) {
            let items = completion::complete(&doc.source, &doc.project, pos);
            Ok(Some(CompletionResponse::Array(items)))
        } else {
            Ok(None)
        }
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let pos = params.text_document_position_params.position;
        let docs = lock_documents(&self.documents);

        if let Some(doc) = docs.cache.get(&uri) {
            Ok(hover::hover_info(&doc.source, &doc.project, pos))
        } else {
            Ok(None)
        }
    }
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(Documents::default()),
            publication: tokio::sync::Mutex::new(()),
        }
    }
    #[cfg(test)]
    async fn update_document_with_limits(
        &self,
        uri: &str,
        source: &str,
        limits: oc_dsl::source_map::SourceLimits,
    ) {
        let admission = lock_documents(&self.documents).next_test_admission(uri);
        if let Some(lease) = admission.and_then(|admission| self.admit_document(uri, admission)) {
            self.run_update(lease, source, limits, &self.client).await;
        }
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);

    Server::new(stdin, stdout, socket).serve(service).await;
}
