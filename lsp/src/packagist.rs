//! Packagist v2 metadata client with a small TTL cache.
//!
//! Endpoint: `https://repo.packagist.org/p2/{vendor}/{package}.json` — static,
//! cache-friendly JSON dumped newest-first.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use semver::Version;
use serde::Deserialize;

use crate::constraint::normalize_version;

const DEFAULT_REGISTRY: &str = "https://repo.packagist.org";
const DEFAULT_TTL_SECS: u64 = 300;

/// The latest stable release of a package: display string, parsed version, and
/// a browsable source-repository URL when one is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub display: String,
    pub version: Version,
    pub repository: Option<String>,
}

/// A single published version, as offered in completion. `stable` is false for
/// any pre-release/dev tag (anything containing `-`, e.g. `-RC1`, `x-dev`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    pub display: String,
    pub stable: bool,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: HashMap<String, Vec<VersionEntry>>,
}

#[derive(Debug, Deserialize)]
struct VersionEntry {
    version: String,
    #[serde(default)]
    source: Option<Source>,
}

#[derive(Debug, Deserialize)]
struct Source {
    url: Option<String>,
}

/// Pick the highest stable release from a package's version list. Pre-release
/// tags (anything containing `-`, e.g. `-RC1`, `-beta2`, `-dev`) are ignored.
///
/// Composer minifies the metadata: fields absent from an entry inherit from the
/// previous entry in array order, so we carry the last-seen `source` forward.
fn select_latest_stable(entries: &[VersionEntry]) -> Option<Release> {
    let mut inherited_repo: Option<String> = None;
    let mut best: Option<Release> = None;

    for entry in entries {
        if let Some(url) = entry.source.as_ref().and_then(|s| s.url.as_deref()) {
            inherited_repo = normalize_repo_url(url);
        }

        let display = entry.version.trim_start_matches(['v', 'V']);
        if display.contains('-') {
            continue;
        }
        let Some(version) = normalize_version(display) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| version > b.version) {
            best = Some(Release {
                display: display.to_string(),
                version,
                repository: inherited_repo.clone(),
            });
        }
    }
    best
}

/// List every published version newest-first (Packagist's native order),
/// tagging each as stable or pre-release. The leading `v`/`V` is stripped so
/// the display matches `select_latest_stable`.
fn collect_versions(entries: &[VersionEntry]) -> Vec<VersionInfo> {
    entries
        .iter()
        .filter_map(|entry| {
            let display = entry.version.trim().trim_start_matches(['v', 'V']);
            if display.is_empty() {
                return None;
            }
            Some(VersionInfo {
                display: display.to_string(),
                stable: !display.contains('-'),
            })
        })
        .collect()
}

/// Normalize a VCS URL into a browsable `https` repository link: strip a
/// trailing `.git` and rewrite `git@host:owner/repo` SSH form to `https`.
fn normalize_repo_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let https = if let Some(rest) = url.strip_prefix("git@") {
        // git@github.com:owner/repo -> https://github.com/owner/repo
        format!("https://{}", rest.replacen(':', "/", 1))
    } else {
        url.to_string()
    };
    Some(https.strip_suffix(".git").unwrap_or(&https).to_string())
}

#[derive(Clone)]
struct CacheEntry {
    fetched_at: Instant,
    release: Option<Release>,
    versions: Vec<VersionInfo>,
}

/// HTTP client + per-package TTL cache. Network/parse failures resolve to
/// `None` (treated as "no info"), never an error that aborts a whole pass.
pub struct Packagist {
    client: reqwest::Client,
    registry: String,
    ttl: Duration,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl Packagist {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("composer-update-checker-lsp")
                .build()
                .unwrap_or_default(),
            registry: DEFAULT_REGISTRY.to_string(),
            ttl: Duration::from_secs(DEFAULT_TTL_SECS),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Latest stable release for `vendor/package`, served from cache when fresh.
    pub async fn latest_stable(&self, name: &str) -> Option<Release> {
        self.load(name).await.release
    }

    /// All published versions for `vendor/package`, newest-first, served from
    /// cache when fresh.
    pub async fn versions(&self, name: &str) -> Vec<VersionInfo> {
        self.load(name).await.versions
    }

    /// Resolve a package's cache entry, fetching (and caching) once when stale.
    async fn load(&self, name: &str) -> CacheEntry {
        if let Some(entry) = self.cached(name) {
            if entry.fetched_at.elapsed() < self.ttl {
                return entry;
            }
        }

        let entry = self.fetch(name).await;
        self.store(name, entry.clone());
        entry
    }

    fn cached(&self, name: &str) -> Option<CacheEntry> {
        self.cache.lock().ok()?.get(name).cloned()
    }

    fn store(&self, name: &str, entry: CacheEntry) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(name.to_string(), entry);
        }
    }

    /// Fetch and parse package metadata once into both the latest stable release
    /// and the full version list. Network/parse failures yield an empty entry
    /// (no release, no versions) — cached like any other result.
    async fn fetch(&self, name: &str) -> CacheEntry {
        let (release, versions) = self.fetch_metadata(name).await.unwrap_or_default();
        CacheEntry {
            fetched_at: Instant::now(),
            release,
            versions,
        }
    }

    async fn fetch_metadata(&self, name: &str) -> Option<(Option<Release>, Vec<VersionInfo>)> {
        let url = format!("{}/p2/{}.json", self.registry, name);
        let response = self.client.get(&url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let metadata = response.json::<Metadata>().await.ok()?;
        let entries = metadata.packages.get(name)?;
        Some((select_latest_stable(entries), collect_versions(entries)))
    }
}

impl Default for Packagist {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(versions: &[&str]) -> Vec<VersionEntry> {
        versions
            .iter()
            .map(|v| VersionEntry {
                version: v.to_string(),
                source: None,
            })
            .collect()
    }

    fn source(url: &str) -> Option<Source> {
        Some(Source {
            url: Some(url.to_string()),
        })
    }

    #[test]
    fn picks_highest_stable_ignoring_prereleases() {
        let list = entries(&["v12.1.0", "v12.0.0", "v12.2.0-RC1", "v12.2.0-beta1"]);
        let latest = select_latest_stable(&list).unwrap();
        assert_eq!(latest.display, "12.1.0");
        assert_eq!(latest.version, Version::new(12, 1, 0));
    }

    #[test]
    fn ignores_dev_and_alpha_tags() {
        let list = entries(&["9.0.0", "10.0.0-dev", "10.0.0-alpha3"]);
        let latest = select_latest_stable(&list).unwrap();
        assert_eq!(latest.display, "9.0.0");
    }

    #[test]
    fn none_when_only_prereleases() {
        let list = entries(&["1.0.0-beta1", "1.0.0-RC1"]);
        assert_eq!(select_latest_stable(&list), None);
    }

    #[test]
    fn out_of_order_list_still_returns_max() {
        let list = entries(&["8.0.0", "12.5.0", "10.2.0"]);
        assert_eq!(select_latest_stable(&list).unwrap().display, "12.5.0");
    }

    #[test]
    fn captures_repository_from_source() {
        let mut list = entries(&["12.1.0", "12.0.0"]);
        list[0].source = source("https://github.com/laravel/framework.git");
        let latest = select_latest_stable(&list).unwrap();
        assert_eq!(
            latest.repository.as_deref(),
            Some("https://github.com/laravel/framework")
        );
    }

    #[test]
    fn inherits_source_for_minified_entries() {
        // Only the newest entry carries `source`; older ones inherit it.
        let mut list = entries(&["12.1.0", "12.0.0"]);
        list[0].source = source("git@github.com:acme/widget.git");
        let latest = select_latest_stable(&list).unwrap();
        assert_eq!(
            latest.repository.as_deref(),
            Some("https://github.com/acme/widget")
        );
    }

    #[test]
    fn collects_all_versions_newest_first_with_stability() {
        let list = entries(&["v7.12.x-dev", "7.12.1", "7.12.0", "7.11.2-RC1"]);
        let versions = collect_versions(&list);
        let displays: Vec<&str> = versions.iter().map(|v| v.display.as_str()).collect();
        // Order preserved (Packagist serves newest-first); leading `v` stripped.
        assert_eq!(displays, ["7.12.x-dev", "7.12.1", "7.12.0", "7.11.2-RC1"]);
        // Stability tracks the presence of a pre-release/dev suffix.
        let stable: Vec<bool> = versions.iter().map(|v| v.stable).collect();
        assert_eq!(stable, [false, true, true, false]);
    }

    #[test]
    fn normalizes_ssh_and_strips_git_suffix() {
        assert_eq!(
            normalize_repo_url("git@github.com:acme/widget.git").as_deref(),
            Some("https://github.com/acme/widget")
        );
        assert_eq!(
            normalize_repo_url("https://gitlab.com/acme/widget").as_deref(),
            Some("https://gitlab.com/acme/widget")
        );
        assert_eq!(normalize_repo_url("").as_deref(), None);
    }
}
