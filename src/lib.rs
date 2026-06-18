use zed_extension_api::{self as zed, settings::LspSettings, Command, LanguageServerId, Result};

const SERVER_BINARY: &str = "composer-update-checker-lsp";

struct ComposerUpdatesExtension {
    cached_binary_path: Option<String>,
}

impl ComposerUpdatesExtension {
    /// Resolve the language-server binary path.
    ///
    /// MVP strategy (no auto-download): prefer a binary on the worktree `PATH`,
    /// then fall back to a previously cached path. If neither exists we return
    /// an error instructing the user to build and install the LSP (see README).
    fn language_server_binary_path(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Some(path) = worktree.which(SERVER_BINARY) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        if let Some(path) = &self.cached_binary_path {
            return Ok(path.clone());
        }

        Err(format!(
            "`{SERVER_BINARY}` was not found on PATH. Build it with \
             `cargo build --release -p {SERVER_BINARY}` and symlink \
             `target/release/{SERVER_BINARY}` into a directory on your PATH \
             (see the extension README)."
        ))
    }
}

impl zed::Extension for ComposerUpdatesExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Command> {
        let binary_path = self.language_server_binary_path(language_server_id, worktree)?;

        Ok(Command {
            command: binary_path,
            args: vec![],
            env: vec![],
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.initialization_options))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.settings))
    }
}

zed::register_extension!(ComposerUpdatesExtension);
