//! Stable-release checks shared by the GUI and terminal interface.

use std::{path::Path, sync::mpsc, thread, time::Duration};

use serde::Deserialize;
use ureq::ResponseExt;

const DEFAULT_REPO: &str = "coanor/k3";

/// The result of checking a stable release without modifying an installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseStatus {
    Current,
    Available { version: String, url: String },
}

/// Resolve the release repository from an explicit override or installation metadata.
///
/// # Errors
/// Returns an error if an explicit override or installed repository is malformed.
pub fn release_repository() -> Result<String, String> {
    if let Ok(repo) = std::env::var("K3_RELEASE_REPO") {
        return validate_repository(repo);
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(root) = executable.parent()
    {
        for name in ["installation-state.json", "install-manifest.json"] {
            let path = root.join(name);
            if path.is_file() {
                return installed_repository(&path);
            }
        }
    }
    Ok(DEFAULT_REPO.into())
}

fn installed_repository(path: &Path) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Installation {
        repo: Option<String>,
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let metadata: Installation =
        serde_json::from_reader(file).map_err(|error| error.to_string())?;
    validate_repository(metadata.repo.unwrap_or_else(|| DEFAULT_REPO.into()))
}

fn validate_repository(repo: String) -> Result<String, String> {
    let parts = repo.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
    {
        return Err("Release repository must use owner/repo format".into());
    }
    Ok(repo)
}

/// Check the latest stable GitHub release with a bounded network timeout.
///
/// # Errors
/// Returns an error on network failure, API rate limits, or invalid release metadata.
pub fn check_release(repo: &str, current: &str) -> Result<ReleaseStatus, String> {
    validate_repository(repo.to_owned())?;
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build(),
    );
    with_public_fallback(
        || check_api_release(&agent, repo, current),
        || {
            let response = agent
                .head(format!("https://github.com/{repo}/releases/latest"))
                .header("User-Agent", "K3-release-check")
                .call()
                .map_err(|error| error.to_string())?;
            release_from_url(repo, current, &response.get_uri().to_string())
        },
    )
}

fn with_public_fallback(
    api: impl FnOnce() -> Result<ReleaseStatus, String>,
    public: impl FnOnce() -> Result<ReleaseStatus, String>,
) -> Result<ReleaseStatus, String> {
    api().or_else(|api_error| {
        public().map_err(|error| format!("{api_error}; public release check: {error}"))
    })
}

fn check_api_release(
    agent: &ureq::Agent,
    repo: &str,
    current: &str,
) -> Result<ReleaseStatus, String> {
    let mut response = agent
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("User-Agent", "K3-release-check")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|error| error.to_string())?;
    let body = response
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_string()
        .map_err(|error| error.to_string())?;
    release_from_json(repo, current, &body)
}

fn release_from_url(repo: &str, current: &str, url: &str) -> Result<ReleaseStatus, String> {
    let prefix = format!("https://github.com/{repo}/releases/tag/v");
    let latest = url
        .strip_prefix(&prefix)
        .ok_or("Unexpected public release redirect")?;
    compare_release(repo, current, latest)
}

fn version(value: &str) -> Result<[u64; 3], String> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("Release version must use numeric major.minor.patch format".into());
    }
    Ok([
        parts[0].parse().map_err(|_| "Invalid major version")?,
        parts[1].parse().map_err(|_| "Invalid minor version")?,
        parts[2].parse().map_err(|_| "Invalid patch version")?,
    ])
}

fn release_from_json(repo: &str, current: &str, source: &str) -> Result<ReleaseStatus, String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        draft: bool,
        prerelease: bool,
    }
    let release: Release = serde_json::from_str(source).map_err(|error| error.to_string())?;
    if release.draft || release.prerelease {
        return Err("Latest release is not a stable published release".into());
    }
    let latest = release
        .tag_name
        .strip_prefix('v')
        .ok_or("Release tag must start with v")?;
    compare_release(repo, current, latest)
}

fn compare_release(repo: &str, current: &str, latest: &str) -> Result<ReleaseStatus, String> {
    if version(latest)? > version(current)? {
        Ok(ReleaseStatus::Available {
            version: latest.into(),
            url: format!("https://github.com/{repo}/releases/tag/v{latest}"),
        })
    } else {
        Ok(ReleaseStatus::Current)
    }
}

/// Start a background check. Frontends can poll the receiver without blocking audio or input.
#[must_use]
pub fn start_release_check(current: String) -> mpsc::Receiver<Result<ReleaseStatus, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let outcome = release_repository().and_then(|repo| check_release(&repo, &current));
        let _ = sender.send(outcome);
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::{
        ReleaseStatus, release_from_json, release_from_url, validate_repository,
        with_public_fallback,
    };

    #[test]
    fn numeric_versions_handle_double_digits_and_older_releases() {
        let metadata = r#"{"tag_name":"v0.10.0","draft":false,"prerelease":false}"#;
        assert!(
            matches!(release_from_json("org/repo", "0.9.9", metadata).unwrap(), ReleaseStatus::Available { version, url } if version == "0.10.0" && url == "https://github.com/org/repo/releases/tag/v0.10.0")
        );
        assert_eq!(
            release_from_json("org/repo", "1.0.0", metadata).unwrap(),
            ReleaseStatus::Current
        );
        assert_eq!(
            release_from_json("org/repo", "0.10.0", metadata).unwrap(),
            ReleaseStatus::Current
        );
    }

    #[test]
    fn unstable_or_malformed_release_metadata_is_rejected() {
        for metadata in [
            r#"{"tag_name":"v1.0.0","draft":true,"prerelease":false}"#,
            r#"{"tag_name":"v1.0.0","draft":false,"prerelease":true}"#,
            r#"{"tag_name":"v1.0.0-rc1","draft":false,"prerelease":false}"#,
            r#"{"tag_name":"https://untrusted.example","draft":false,"prerelease":false}"#,
            "{}",
        ] {
            assert!(release_from_json("org/repo", "0.1.2", metadata).is_err());
        }
    }

    #[test]
    fn rate_limited_api_falls_back_to_the_public_latest_release() {
        let result = with_public_fallback(
            || Err("GitHub API rate limit exceeded".into()),
            || {
                release_from_url(
                    "org/repo",
                    "0.1.2",
                    "https://github.com/org/repo/releases/tag/v0.2.0",
                )
            },
        )
        .unwrap();
        assert!(matches!(result, ReleaseStatus::Available { version, .. } if version == "0.2.0"));
        assert_eq!(
            with_public_fallback(
                || Ok(ReleaseStatus::Current),
                || panic!("fallback should not be called")
            ),
            Ok(ReleaseStatus::Current)
        );
        for url in [
            "http://github.com/org/repo/releases/tag/v1.0.0",
            "https://other.example/org/repo/releases/tag/v1.0.0",
            "https://github.com/org/other/releases/tag/v1.0.0",
            "https://github.com/org/repo/releases/tag/v1.0.0-rc1",
        ] {
            assert!(release_from_url("org/repo", "0.1.2", url).is_err());
        }
    }

    #[test]
    fn repository_cannot_change_the_request_host_or_inject_a_path() {
        for repo in [
            "org/repo/extra",
            "../repo",
            "org/..",
            "org/repo?x=y",
            "org/repo\n",
            "https://example.com",
        ] {
            assert!(validate_repository(repo.into()).is_err());
        }
    }
}
