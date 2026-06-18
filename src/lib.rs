use std::fs;

use zed_extension_api::{
    self as zed, settings::LspSettings, Architecture, Command, DownloadedFileType,
    LanguageServerId, LanguageServerInstallationStatus, Os, Result,
};

const SERVER_BINARY: &str = "composer-update-checker-lsp";
/// GitHub repository that publishes the language-server release assets.
const RELEASE_REPO: &str = "babul/zed-composer-update-checker";

struct ComposerUpdatesExtension {
    cached_binary_path: Option<String>,
}

impl ComposerUpdatesExtension {
    /// Resolve the language-server binary path.
    ///
    /// Order: (1) a binary on the worktree `PATH` (the local dev workflow),
    /// (2) a previously downloaded binary that still exists, then (3) download
    /// the release asset matching this extension's version and platform.
    fn language_server_binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Some(path) = worktree.which(SERVER_BINARY) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok() {
                return Ok(path.clone());
            }
        }

        let path = self.download_binary(language_server_id)?;
        self.cached_binary_path = Some(path.clone());
        Ok(path)
    }

    /// Download (once) the release asset pinned to this extension's version and
    /// return the path to the extracted binary.
    fn download_binary(&self, language_server_id: &LanguageServerId) -> Result<String> {
        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let version = env!("CARGO_PKG_VERSION");
        let tag = format!("v{version}");
        let release = zed::github_release_by_tag_name(RELEASE_REPO, &tag)?;

        let (target, archive) = release_target()?;
        let asset_name = format!("{SERVER_BINARY}-{target}.{}", archive.extension());
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no release asset named `{asset_name}` in {tag}"))?;

        let version_dir = format!("{SERVER_BINARY}-{version}");
        let binary_path = format!("{version_dir}/{SERVER_BINARY}{}", archive.binary_suffix());

        if fs::metadata(&binary_path).is_err() {
            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Downloading,
            );
            zed::download_file(&asset.download_url, &version_dir, archive.file_type())
                .map_err(|e| format!("failed to download {asset_name}: {e}"))?;
            zed::make_file_executable(&binary_path)?;
            remove_stale_versions(&version_dir);
        }

        Ok(binary_path)
    }
}

/// Archive format of a release asset, derived from the target OS.
enum Archive {
    TarGz,
    Zip,
}

impl Archive {
    fn extension(&self) -> &'static str {
        match self {
            Archive::TarGz => "tar.gz",
            Archive::Zip => "zip",
        }
    }

    fn binary_suffix(&self) -> &'static str {
        match self {
            Archive::TarGz => "",
            Archive::Zip => ".exe",
        }
    }

    fn file_type(&self) -> DownloadedFileType {
        match self {
            Archive::TarGz => DownloadedFileType::GzipTar,
            Archive::Zip => DownloadedFileType::Zip,
        }
    }
}

/// Map the current platform to its Rust target triple (matching the release
/// asset names) and archive format. Errors on unsupported platforms.
fn release_target() -> Result<(&'static str, Archive)> {
    let (os, arch) = zed::current_platform();
    let target = match (os, arch) {
        (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
        (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
        (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-musl",
        (Os::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-musl",
        (Os::Windows, Architecture::X8664) => "x86_64-pc-windows-msvc",
        _ => return Err(format!("unsupported platform: {os:?} {arch:?}")),
    };
    let archive = match os {
        Os::Windows => Archive::Zip,
        _ => Archive::TarGz,
    };
    Ok((target, archive))
}

/// Remove previously downloaded version directories, keeping only `keep`.
fn remove_stale_versions(keep: &str) {
    let Ok(entries) = fs::read_dir(".") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&format!("{SERVER_BINARY}-")) && name != keep {
            let _ = fs::remove_dir_all(entry.path());
        }
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
