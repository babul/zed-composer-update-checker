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
use packagist::{Packagist, Release, VersionInfo};

/// Maximum concurrent Packagist requests per check pass.
const MAX_CONCURRENCY: usize = 10;

/// Command id for the Code Lens "Update to …" action.
const APPLY_UPDATE_COMMAND: &str = "composer-update-checker.applyUpdate";

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
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![APPLY_UPDATE_COMMAND.to_string()],
                    ..Default::default()
                }),
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
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Update {} to {}", dep.name, new_constraint),
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(replace_edit(uri.clone(), dep.value_range, new_constraint)),
                ..Default::default()
            }));
        }

        Ok(Some(actions))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        if !is_composer_manifest(&uri) {
            return Ok(None);
        }
        let Some(text) = self.document(&uri) else {
            return Ok(None);
        };

        let position = params.text_document_position.position;
        for dep in parse_dependencies(&text) {
            if !position_in_range(position, &dep.value_range) {
                continue;
            }
            let versions = self.packagist.versions(&dep.name).await;
            return Ok(Some(CompletionResponse::Array(completion_items(
                &dep, &versions,
            ))));
        }
        Ok(None)
    }

    async fn code_lens(
        &self,
        params: CodeLensParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        if !is_composer_manifest(&uri) {
            return Ok(None);
        }
        let Some(text) = self.document(&uri) else {
            return Ok(None);
        };

        let mut lenses = Vec::new();
        for dep in parse_dependencies(&text) {
            let Some(latest) = self.packagist.latest_stable(&dep.name).await else {
                continue;
            };
            if !is_outdated(&dep.constraint, &latest.version) {
                continue;
            }
            let new_constraint = suggested_constraint(&dep.constraint, &latest.display);
            lenses.push(update_lens(&uri, &dep, &new_constraint));
        }
        Ok(Some(lenses))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        if params.command != APPLY_UPDATE_COMMAND {
            return Ok(None);
        }
        if let Some(args) = params.arguments.into_iter().next() {
            if let Ok(update) = serde_json::from_value::<UpdateArgs>(args) {
                let edit = replace_edit(update.uri, update.range, update.new_text);
                let _ = self.client.apply_edit(edit).await;
            }
        }
        Ok(None)
    }
}

/// Arguments carried by the `applyUpdate` Code Lens command.
#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateArgs {
    uri: Url,
    range: Range,
    new_text: String,
}

/// Single-edit `WorkspaceEdit` replacing `range` in `uri` with `new_text`.
/// Shared by the code action and the Code Lens command.
fn replace_edit(uri: Url, range: Range, new_text: String) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: Some(HashMap::from([(uri, vec![TextEdit { range, new_text }])])),
        ..Default::default()
    }
}

/// Build a "⬆ Update to …" Code Lens that, when clicked, replaces the
/// dependency's constraint with `new_constraint`.
fn update_lens(uri: &Url, dep: &Dependency, new_constraint: &str) -> CodeLens {
    let args = UpdateArgs {
        uri: uri.clone(),
        range: dep.value_range,
        new_text: new_constraint.to_string(),
    };
    CodeLens {
        range: dep.value_range,
        command: Some(Command {
            title: format!("⬆ Update to {new_constraint}"),
            command: APPLY_UPDATE_COMMAND.to_string(),
            arguments: serde_json::to_value(args).ok().map(|v| vec![v]),
        }),
        data: None,
    }
}

/// Completion items offering every published version for `dep`, newest-first.
/// Each item inserts an operator-preserving constraint (`^7.12` → `^7.12.1`);
/// the latest stable is preselected.
fn completion_items(dep: &Dependency, versions: &[VersionInfo]) -> Vec<CompletionItem> {
    let latest_stable = versions.iter().position(|v| v.stable);
    versions
        .iter()
        .enumerate()
        .map(|(i, version)| {
            let new_text = suggested_constraint(&dep.constraint, &version.display);
            CompletionItem {
                label: version.display.clone(),
                kind: Some(CompletionItemKind::VALUE),
                detail: Some(
                    if version.stable { "stable" } else { "pre-release" }.to_string(),
                ),
                // Match the candidate against the value being replaced (e.g.
                // `^7.12`) rather than its bare label, or the operator prefix
                // would filter every item out.
                filter_text: Some(new_text.clone()),
                sort_text: Some(format!("{i:06}")),
                preselect: Some(Some(i) == latest_stable),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: dep.value_range,
                    new_text,
                })),
                ..Default::default()
            }
        })
        .collect()
}

/// True when `position` falls within `range` (constraints are single-line, so a
/// same-line column test suffices; the end is inclusive so the cursor at the
/// closing quote still completes).
fn position_in_range(position: Position, range: &Range) -> bool {
    position.line == range.start.line
        && position.character >= range.start.character
        && position.character <= range.end.character
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(constraint: &str) -> Dependency {
        Dependency {
            name: "guzzlehttp/guzzle".to_string(),
            constraint: constraint.to_string(),
            value_range: Range {
                start: Position::new(3, 26),
                end: Position::new(3, 32),
            },
            dev: false,
        }
    }

    fn version(display: &str, stable: bool) -> VersionInfo {
        VersionInfo {
            display: display.to_string(),
            stable,
        }
    }

    fn edit_text(item: &CompletionItem) -> &str {
        match item.text_edit.as_ref().unwrap() {
            CompletionTextEdit::Edit(edit) => &edit.new_text,
            _ => panic!("expected an Edit text_edit"),
        }
    }

    #[test]
    fn completion_preserves_operator_and_preselects_latest_stable() {
        let versions = [
            version("7.12.x-dev", false),
            version("7.12.1", true),
            version("7.12.0", true),
        ];
        let items = completion_items(&dep("^7.11"), &versions);

        // One item per version, newest-first order held via sort_text.
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].sort_text.as_deref(), Some("000000"));
        assert_eq!(items[2].sort_text.as_deref(), Some("000002"));

        // Operator preserved in both the inserted text and the filter text.
        assert_eq!(edit_text(&items[1]), "^7.12.1");
        assert_eq!(items[1].filter_text.as_deref(), Some("^7.12.1"));

        // The first stable entry is preselected; the dev tag is not.
        assert_eq!(items[1].preselect, Some(true));
        assert_eq!(items[0].preselect, Some(false));
        assert_eq!(items[0].detail.as_deref(), Some("pre-release"));
        assert_eq!(items[1].detail.as_deref(), Some("stable"));
    }

    #[test]
    fn completion_keeps_exact_pin_exact() {
        let items = completion_items(&dep("7.11.2"), &[version("7.12.1", true)]);
        assert_eq!(edit_text(&items[0]), "7.12.1");
    }

    #[test]
    fn position_in_range_is_inclusive_of_both_ends() {
        let range = Range {
            start: Position::new(3, 26),
            end: Position::new(3, 32),
        };
        assert!(position_in_range(Position::new(3, 26), &range));
        assert!(position_in_range(Position::new(3, 29), &range));
        assert!(position_in_range(Position::new(3, 32), &range));
        assert!(!position_in_range(Position::new(3, 25), &range));
        assert!(!position_in_range(Position::new(3, 33), &range));
        assert!(!position_in_range(Position::new(2, 29), &range));
    }

    #[test]
    fn update_lens_carries_round_trippable_args() {
        let uri = Url::parse("file:///app/composer.json").unwrap();
        let lens = update_lens(&uri, &dep("^7.11"), "^7.12.1");
        let command = lens.command.unwrap();
        assert_eq!(command.command, APPLY_UPDATE_COMMAND);
        assert_eq!(command.title, "⬆ Update to ^7.12.1");

        let arg = command.arguments.unwrap().into_iter().next().unwrap();
        let parsed: UpdateArgs = serde_json::from_value(arg).unwrap();
        assert_eq!(parsed.uri, uri);
        assert_eq!(parsed.new_text, "^7.12.1");
        assert_eq!(parsed.range, dep("^7.11").value_range);
    }
}
