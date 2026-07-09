use std::process::Command;

use anyhow::{Context, Result};

/// Compare two Debian version strings using `dpkg --compare-versions`.
/// Returns true if `upstream` is strictly greater than `threshold`.
pub fn is_newer(upstream: &str, threshold: &str) -> Result<bool> {
    let status = Command::new("dpkg")
        .args(["--compare-versions", upstream, "gt", threshold])
        .status()
        .context("Failed to execute [cmd]dpkg --compare-versions[/]")?;

    Ok(status.success())
}

/// Strip the Debian revision suffix (the last `-N` component) from a version string.
/// e.g. "1.2.3-2" -> "1.2.3", "1.2.3" -> "1.2.3", "2:1.2.3-1" -> "2:1.2.3"
pub fn strip_debian_revision(version: &str) -> &str {
    match version.rfind('-') {
        Some(pos) => &version[..pos],
        None => version,
    }
}

/// Extract the Debian revision number from a version string.
/// e.g. "1.2.3-2" -> Some(2), "1.2.3" -> None
pub fn debian_revision(version: &str) -> Option<u32> {
    version
        .rfind('-')
        .and_then(|pos| version[pos + 1..].parse().ok())
}

/// Strip packaging suffixes (`~N`, `~N~tag`, legacy `-N`) and return the upstream part.
pub fn strip_upstream(version: &str) -> &str {
    let tilde_count = version.matches('~').count();

    if tilde_count >= 2
        && let Some(rev_start) = version
            .rfind('~')
            .and_then(|tag_pos| version[..tag_pos].rfind('~'))
    {
        return &version[..rev_start];
    }

    if tilde_count == 1
        && let Some((upstream, suffix)) = version.rsplit_once('~')
        && suffix.chars().all(|c| c.is_ascii_digit())
    {
        return upstream;
    }

    strip_debian_revision(version)
}

/// Extract the packaging revision from `~N`, `~N~tag`, or legacy `-N` formats.
pub fn packaging_revision(version: &str) -> Option<u32> {
    let tilde_count = version.matches('~').count();

    if tilde_count >= 2 {
        let without_tag = version.rsplit_once('~')?.0;
        return without_tag.rsplit_once('~')?.1.parse().ok();
    }

    if tilde_count == 1 {
        let suffix = version.rsplit_once('~')?.1;
        if suffix.chars().all(|c| c.is_ascii_digit()) {
            return suffix.parse().ok();
        }
    }

    debian_revision(version)
}

/// Format a package version as `{upstream}~{revision}`.
pub fn format_pkg_version(upstream: &str, revision: u32) -> String {
    format!("{upstream}~{revision}")
}

/// Append a tilde tag suffix (e.g. `1.2.3~1` + `myrepo` -> `1.2.3~1~myrepo`).
pub fn append_version_tag(version: &str, tag: &str) -> Result<String> {
    if tag.is_empty() {
        crate::error_msg!("--tag must not be empty");
    }
    if tag.contains('~') {
        crate::error_msg!("--tag must not contain '~' (it is added automatically)");
    }
    Ok(format!("{version}~{tag}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_debian_revision_handles_revision_and_no_revision() {
        assert_eq!(strip_debian_revision("1.2.3-2"), "1.2.3");
        assert_eq!(strip_debian_revision("1.2.3"), "1.2.3");
        assert_eq!(strip_debian_revision("2:1.2.3-1"), "2:1.2.3");
    }

    #[test]
    fn debian_revision_extracts_numeric_suffix() {
        assert_eq!(debian_revision("1.2.3-2"), Some(2));
        assert_eq!(debian_revision("1.2.3"), None);
        assert_eq!(debian_revision("1.2.3-alpha"), None);
    }

    #[test]
    fn strip_upstream_handles_tilde_and_legacy_formats() {
        assert_eq!(strip_upstream("1.2.3~4"), "1.2.3");
        assert_eq!(strip_upstream("1.2.3~4~staging"), "1.2.3");
        assert_eq!(strip_upstream("1.2.3-4"), "1.2.3");
    }

    #[test]
    fn packaging_revision_handles_tilde_and_legacy_formats() {
        assert_eq!(packaging_revision("1.2.3~4"), Some(4));
        assert_eq!(packaging_revision("1.2.3~4~staging"), Some(4));
        assert_eq!(packaging_revision("1.2.3-4"), Some(4));
        assert_eq!(packaging_revision("1.2.3"), None);
    }

    #[test]
    fn format_pkg_version_builds_expected_string() {
        assert_eq!(format_pkg_version("1.2.3", 7), "1.2.3~7");
    }

    #[test]
    fn append_version_tag_appends_and_validates() {
        assert_eq!(
            append_version_tag("1.2.3~1", "myrepo").expect("tag should append"),
            "1.2.3~1~myrepo"
        );
        assert!(append_version_tag("1.2.3~1", "").is_err());
        assert!(append_version_tag("1.2.3~1", "bad~tag").is_err());
    }
}
