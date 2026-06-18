//! `composer-update-checker-lsp` — a language server that flags outdated
//! Composer dependencies in `composer.json` and offers version-bump code
//! actions. Latest versions come from the Packagist v2 metadata API.

mod constraint;
mod manifest;
mod packagist;

use std::collections::HashMap;
use std::sync::Mutex;

use futures::stream::{self, StreamExt};
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use constraint::{is_outdated, suggested_constraint};
use manifest::{parse_dependencies, Dependency};
use packagist::{Packagist, Release};

/// Maximum concurrent Packagist requests per check pass.
const MAX_CONCURRENCY: usize = 10;

struct Backend {
    client: Client,
    packagist: Packagist,
    /// Latest full text per open document, keyed by URI.
    documents: Mutex<HashMap<Url, String>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            packagist: Packagist::new(),
            documents: Mutex::new(HashMap::new()),
        }
    }

    fn store_document(&self, uri: Url, text: String) {
        if let Ok(mut docs) = self.documents.lock() {
            docs.insert(uri, text);
        }
    }

    fn document(&self, uri: &Url) -> Option<String> {
        self.documents.lock().ok()?.get(uri).cloned()
    }

    fn forget_document(&self, uri: &Url) {
        if let Ok(mut docs) = self.documents.lock() {
            docs.remove(uri);
        }
    }

    /// Re-check a document and publish diagnostics. No-op (clears diagnostics)
    /// for any JSON file that is not a `composer.json`.
    async fn refresh(&self, uri: Url, text: String) {
        if !is_composer_manifest(&uri) {
            self.client.publish_diagnostics(uri, vec![], None).await;
            return;
        }

        let deps = parse_dependencies(&text);
        let diagnostics = stream::iter(deps)
            .map(|dep| async move {
                let latest = self.packagist.latest_stable(&dep.name).await?;
                if is_outdated(&dep.constraint, &latest.version) {
                    Some(Diagnostic {
                        range: dep.value_range,
                        severity: Some(DiagnosticSeverity::HINT),
                        source: Some("composer-update-checker".to_string()),
                        code: Some(NumberOrString::String(dep.name.clone())),
                        code_description: Url::parse(&packagist_url(&dep.name))
                            .ok()
                            .map(|href| CodeDescription { href }),
                        message: format!(
                            "Update available: {} → {}",
                            dep.constraint, latest.display
                        ),
                        ..Default::default()
                    })
                } else {
                    None
                }
            })
            .buffer_unordered(MAX_CONCURRENCY)
            .filter_map(|diagnostic| async move { diagnostic })
            .collect::<Vec<_>>()
            .await;

        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "composer-update-checker-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "composer-update-checker-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.store_document(uri.clone(), text.clone());
        self.refresh(uri, text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // Full sync: the last change carries the complete document text.
        if let Some(change) = params.content_changes.into_iter().last() {
            self.store_document(uri.clone(), change.text.clone());
            self.refresh(uri, change.text).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(text) = self.document(&uri) {
            self.refresh(uri, text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.forget_document(&params.text_document.uri);
    }

    async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        if !is_composer_manifest(&uri) {
            return Ok(None);
        }
        let Some(text) = self.document(&uri) else {
            return Ok(None);
        };

        let line = params.text_document_position_params.position.line;
        for dep in parse_dependencies(&text) {
            if dep.value_range.start.line != line {
                continue;
            }
            let latest = self.packagist.latest_stable(&dep.name).await;
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover_markdown(&dep, latest.as_ref()),
                }),
                range: Some(dep.value_range),
            }));
        }
        Ok(None)
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        if !is_composer_manifest(&uri) {
            return Ok(None);
        }
        let Some(text) = self.document(&uri) else {
            return Ok(None);
        };

        let mut actions = Vec::new();
        for dep in parse_dependencies(&text) {
            if !ranges_overlap(&dep.value_range, &params.range) {
                continue;
            }
            let Some(latest) = self.packagist.latest_stable(&dep.name).await else {
                continue;
            };
            if !is_outdated(&dep.constraint, &latest.version) {
                continue;
            }

            let new_constraint = suggested_constraint(&dep.constraint, &latest.display);
            let edit = TextEdit {
                range: dep.value_range,
                new_text: new_constraint.clone(),
            };
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Update {} to {}", dep.name, new_constraint),
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }

        Ok(Some(actions))
    }
}

/// The Packagist package page for a `vendor/package`.
fn packagist_url(name: &str) -> String {
    format!("https://packagist.org/packages/{name}")
}

/// Markdown shown on hover: the package's update status plus links to its
/// Packagist page and (when known) its source repository.
fn hover_markdown(dep: &Dependency, latest: Option<&Release>) -> String {
    let heading = match latest {
        Some(r) if is_outdated(&dep.constraint, &r.version) => {
            format!("**{}** — update available: `{}` → `{}`", dep.name, dep.constraint, r.display)
        }
        Some(r) => format!("**{}** — up to date · latest `{}`", dep.name, r.display),
        None => format!("**{}**", dep.name),
    };

    let mut links = vec![format!("[Packagist]({})", packagist_url(&dep.name))];
    if let Some(repo) = latest.and_then(|r| r.repository.as_deref()) {
        links.push(format!("[Repository]({repo})"));
    }

    format!("{heading}\n\n{}", links.join(" · "))
}

/// True only for files named `composer.json`.
fn is_composer_manifest(uri: &Url) -> bool {
    uri.path()
        .rsplit('/')
        .next()
        .is_some_and(|name| name == "composer.json")
}

/// Line-level overlap test between a dependency's value range and a requested
/// code-action range.
fn ranges_overlap(a: &Range, b: &Range) -> bool {
    a.start.line <= b.end.line && a.end.line >= b.start.line
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
