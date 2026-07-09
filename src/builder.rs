use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::checkrepo;
use crate::config::DebgenConfig;
use crate::download::{AuthTokens, parse_download_url, perform_download};
use crate::version::{
    append_version_tag, format_pkg_version, is_newer, packaging_revision, strip_upstream,
};
use tracing::{debug, info, warn};

const DEBIAN_POLICY_VERSION: &str = "4.7.2";
const DEFAULT_VERSION: &str = "1.0";

pub struct DebPkgBuilder {
    cfg: DebgenConfig,
    buildenv: PathBuf,
    buildroot: PathBuf,
    only_newer: Option<String>,
    increment: bool,
    version_tag: Option<String>,
    keep_sources: bool,
    tokens: AuthTokens,
}

impl DebPkgBuilder {
    pub fn new(
        cfg: DebgenConfig,
        only_newer: Option<String>,
        increment: bool,
        version_tag: Option<String>,
        keep_sources: bool,
        tokens: AuthTokens,
        output: PathBuf,
    ) -> Self {
        let buildenv = output;
        let buildroot = buildenv.join(&cfg.name);
        debug!(
            "Build environment: [path]{}[/], build root: [path]{}[/]",
            buildenv.display(),
            buildroot.display()
        );
        Self {
            cfg,
            buildenv,
            buildroot,
            only_newer,
            increment,
            version_tag,
            keep_sources,
            tokens,
        }
    }

    pub fn build(&mut self) -> Result<()> {
        self.clean_buildenv()?;
        let upstream_version = self.download()?;

        let version = upstream_version.unwrap_or_else(|| DEFAULT_VERSION.to_string());
        debug!("Package version: [version]{}[/]", version);
        self.cfg.interpolate_version(&version);

        let revision = if let Some(ref only_newer_val) = self.only_newer {
            self.check_version(&version, only_newer_val)?
        } else {
            1
        };

        let mut pkg_version = format_pkg_version(&version, revision);

        if let Some(ref tag) = self.version_tag {
            pkg_version = append_version_tag(&pkg_version, tag)?;
            info!(
                "[action]Applying[/] version tag [field]~{}[/] → [version]{}[/]",
                tag, pkg_version
            );
        }

        self.init_pkgenv(&pkg_version)?;
        self.configure()?;
        self.build_pkg()?;

        if !self.keep_sources {
            self.cleanup_sources()?;
        } else {
            info!(
                "[action]Keeping[/] sources in [path]{}[/]",
                self.buildroot.display()
            );
        }

        Ok(())
    }

    /// Compare upstream version against repo/threshold and determine the
    /// packaging revision to use. Returns the revision number (`~N` suffix).
    fn check_version(&self, upstream: &str, only_newer_val: &str) -> Result<u32> {
        let repo_version = self.resolve_threshold(only_newer_val)?;

        let repo_version = match repo_version {
            Some(v) => v,
            None => {
                info!("[skip]Package not found in repository, [action]proceeding with build[/][/]");
                return Ok(1);
            }
        };

        let repo_upstream = strip_upstream(&repo_version);
        info!(
            "Version check: upstream=[version]{}[/], repo=[version]{}[/] (upstream part: [version]{}[/])",
            upstream, repo_version, repo_upstream
        );

        if is_newer(upstream, repo_upstream)? {
            info!(
                "Upstream version [version]{}[/] is newer than repo [version]{}[/], [action]proceeding[/]",
                upstream, repo_upstream
            );
            return Ok(1);
        }

        if upstream == repo_upstream && self.increment {
            let current_rev = packaging_revision(&repo_version).unwrap_or(0);
            let new_rev = current_rev + 1;
            info!(
                "Same upstream version [version]{}[/], [action]incrementing[/] packaging revision: [version]{}[/]",
                upstream,
                format_pkg_version(upstream, new_rev)
            );
            return Ok(new_rev);
        }

        info!(
            "[skip]Upstream version [version]{}[/] is not newer than repo version [version]{}[/], [action]skipping build[/]",
            upstream, repo_upstream
        );
        std::process::exit(0);
    }

    /// Upload built packages via dput using a generated temporary dput.cf.
    pub fn upload(&self, uri: &str) -> Result<()> {
        info!("[action]Uploading[/] packages via [cmd]dput[/]");
        let dput_cfg = generate_dput_cf(uri)?;
        debug!("Generated dput.cf content:\n{}", dput_cfg.content);

        let changes_path = self.find_changes_file()?;
        info!("[action]Uploading[/] [path]{}[/]", changes_path.display());

        let tmp = tempfile::Builder::new()
            .prefix("debgen-dput-")
            .suffix(".cf")
            .tempfile()
            .context("Failed to create temporary [path]dput.cf[/]")?;

        fs::write(tmp.path(), &dput_cfg.content)
            .context("Failed to write temporary [path]dput.cf[/]")?;
        debug!(
            "Temporary dput.cf written to [path]{}[/]",
            tmp.path().display()
        );

        let status = Command::new("dput")
            .args([
                "-c",
                &tmp.path().to_string_lossy(),
                &dput_cfg.host,
                &changes_path.to_string_lossy(),
            ])
            .status()
            .context("Failed to execute [cmd]dput[/]")?;

        if !status.success() {
            crate::error_msg!("[cmd]dput[/] exited with status: {}", status);
        }

        info!("[ok]Upload completed successfully[/]");
        Ok(())
    }

    /// Remove the build directory and all its contents.
    pub fn clean(&self) -> Result<()> {
        if self.buildenv.exists() {
            info!(
                "[action]Cleaning[/] build directory [path]{}[/]",
                self.buildenv.display()
            );
            fs::remove_dir_all(&self.buildenv).context(format!(
                "Failed to remove [path]{}[/]",
                self.buildenv.display()
            ))?;
        } else {
            debug!(
                "Build directory [path]{}[/] does not exist, nothing to clean",
                self.buildenv.display()
            );
        }
        Ok(())
    }

    fn find_changes_file(&self) -> Result<PathBuf> {
        let pattern = format!("{}_", self.cfg.name);
        for entry in fs::read_dir(&self.buildenv).context(format!(
            "Failed to read [path]{}[/]",
            self.buildenv.display()
        ))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&pattern) && name.ends_with(".changes") {
                return Ok(entry.path());
            }
        }
        crate::error_msg!(
            "No .changes file found in [path]{}[/]",
            self.buildenv.display()
        )
    }

    /// Resolve the --only-newer threshold: if it looks like a repo URL, query
    /// it for the current version of this package; otherwise use it as-is.
    /// Returns None if the value is a repo URL and the package is not found.
    fn resolve_threshold(&self, value: &str) -> Result<Option<String>> {
        if value.starts_with("http://") || value.starts_with("https://") {
            let (repo_url, dist, section, arch) = parse_repo_url(value);
            info!(
                "[action]Resolving version[/] from repo [url]{}[/], dist=[field]{}[/], section=[field]{}[/], arch=[field]{}[/], package=[pkg]{}[/]",
                repo_url, dist, section, arch, self.cfg.name
            );
            checkrepo::get_version(&repo_url, &self.cfg.name, &dist, &section, &arch)
        } else {
            Ok(Some(value.to_string()))
        }
    }

    fn clean_buildenv(&self) -> Result<()> {
        if self.buildenv.exists() {
            info!(
                "[action]Removing[/] previously detected buildenv [path]{}[/]",
                self.buildenv.display()
            );
            fs::remove_dir_all(&self.buildenv).context(format!(
                "Failed to remove build environment [path]{}[/]",
                self.buildenv.display()
            ))?;
        }
        Ok(())
    }

    fn cleanup_sources(&self) -> Result<()> {
        if self.buildroot.exists() {
            debug!(
                "[action]Cleaning up[/] source directory [path]{}[/]",
                self.buildroot.display()
            );
            fs::remove_dir_all(&self.buildroot).context(format!(
                "Failed to remove source directory [path]{}[/]",
                self.buildroot.display()
            ))?;
        }
        Ok(())
    }

    fn download(&self) -> Result<Option<String>> {
        info!(
            "[action]Downloading[/] sources/binaries from upstream for [pkg]{}[/]",
            self.cfg.name
        );

        fs::create_dir_all(&self.buildroot).context(format!(
            "Failed to create build root [path]{}[/]",
            self.buildroot.display()
        ))?;

        debug!("Raw location from config: [field]{}[/]", self.cfg.location);
        let parsed = parse_download_url(&self.cfg.location)?;
        debug!("Resolved location: [field]{:?}[/]", parsed);
        let result = perform_download(
            &parsed,
            &self.buildroot,
            self.cfg.flavor.as_deref(),
            &self.tokens,
        )?;
        debug!(
            "Sources extracted to: [path]{}[/]",
            result.extract_path.display()
        );

        if let Some(ref version) = result.version {
            info!("Upstream version: [version]{}[/]", version);
        }

        Ok(result.version)
    }

    fn whoami(&self) -> Result<String> {
        if let Some(ref maintainer) = self.cfg.maintainer {
            return Ok(maintainer.clone());
        }

        let fullname = std::env::var("DEBFULLNAME").ok().or_else(|| {
            users::get_user_by_uid(users::get_current_uid())
                .map(|u| u.name().to_string_lossy().to_string())
        });

        let email = std::env::var("DEBEMAIL")
            .or_else(|_| std::env::var("EMAIL"))
            .unwrap_or_else(|_| {
                let username = users::get_user_by_uid(users::get_current_uid())
                    .map(|u| u.name().to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let host = hostname::get()
                    .map(|h| h.to_string_lossy().to_string())
                    .unwrap_or_else(|_| "localhost".to_string());
                format!("{}@{}", username, host)
            });

        let name = fullname.unwrap_or_else(|| "Unknown".to_string());

        Ok(format!("{} <{}>", name, email))
    }

    fn write_file(&self, path: &Path, data: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context(format!(
                "Failed to create parent directory for [path]{}[/]",
                path.display()
            ))?;
        }
        fs::write(path, data).context(format!("Failed to write [path]{}[/]", path.display()))
    }

    fn write_file_with_mode(&self, path: &Path, data: &str, mode: u32) -> Result<()> {
        self.write_file(path, data)?;
        let perms = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, perms).context(format!(
            "Failed to set permissions on [path]{}[/]",
            path.display()
        ))?;
        Ok(())
    }

    fn init_pkgenv(&self, version: &str) -> Result<()> {
        let debdir = self.buildroot.join("debian");
        info!(
            "[action]Creating[/] build environment from [path]{}[/]",
            self.buildroot.display()
        );
        debug!("Debian directory: [path]{}[/]", debdir.display());

        let now = Utc::now();
        fs::create_dir_all(&debdir).context(format!(
            "Failed to create debian directory [path]{}[/]",
            debdir.display()
        ))?;

        let debsrcdir = debdir.join("source");
        fs::create_dir_all(&debsrcdir).context(format!(
            "Failed to create debian/source directory [path]{}[/]",
            debsrcdir.display()
        ))?;

        self.write_file(&debsrcdir.join("format"), "3.0 (native)")?;

        self.write_file_with_mode(
            &debdir.join("rules"),
            "#!/usr/bin/make -f\n\n%:\n\tdh $@",
            0o755,
        )?;

        let maintainer = self.whoami()?;

        self.write_file(
            &debdir.join("copyright"),
            &format!(
                "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n\
                 Source: {homepage}\n\
                 Upstream-Name: {name}\n\
                 Upstream-Contact: {contact}\n\n\
                 Files:\n *\n\
                 Copyright:\n {year}, {contact}\n\
                 License: {license}",
                homepage = self.cfg.homepage,
                name = self.cfg.name,
                contact = self.cfg.contact,
                year = now.format("%Y"),
                license = self.cfg.license,
            ),
        )?;

        let bdeps_str = self
            .cfg
            .build_depends
            .iter()
            .map(|e| format!(" {},\n", e))
            .collect::<String>();

        let deps_str = self
            .cfg
            .depends
            .iter()
            .map(|e| format!(" {},\n", e))
            .collect::<String>();

        self.write_file(
            &debdir.join("control"),
            &format!(
                "Source: {name}\n\
                 Section: {section}\n\
                 Priority: {priority}\n\
                 Maintainer: {maintainer}\n\
                 Rules-Requires-Root: no\n\
                 Build-Depends:\n debhelper-compat (= 13)\n\
                 {bdeps}\
                 Standards-Version: {policy}\n\
                 Homepage: {homepage}\n\n\
                 Package: {name}\n\
                 Architecture: {arch}\n\
                 Depends:\n ${{misc:Depends}},\n\
                 {deps}\
                 Description: {desc}\n\
                 \x20{desc}\n .\n\
                 \x20{desc}\n .\n\
                 \x20{desc}\n .\n",
                name = self.cfg.name,
                section = self.cfg.section,
                priority = self.cfg.priority,
                maintainer = maintainer,
                bdeps = bdeps_str,
                policy = DEBIAN_POLICY_VERSION,
                homepage = self.cfg.homepage,
                arch = self.cfg.arch,
                deps = deps_str,
                desc = self.cfg.description,
            ),
        )?;

        let rfc2822_date = Utc::now().format("%a, %d %b %Y %H:%M:%S %z").to_string();

        self.write_file(
            &debdir.join("changelog"),
            &format!(
                "{name} ({version}) unstable; urgency=medium\n\n\
                 \x20 New upstream release.\n\n\
                 \x20-- {maintainer}  {date}\n",
                name = self.cfg.name,
                version = version,
                maintainer = maintainer,
                date = rfc2822_date,
            ),
        )?;

        self.write_file(&debdir.join("dirs"), &self.cfg.dirs.join("\n"))?;

        let install_lines: Vec<String> = self
            .cfg
            .files
            .iter()
            .map(|(k, v)| format!("{} {}", k, v))
            .collect();
        self.write_file(&debdir.join("install"), &install_lines.join("\n"))?;

        debug!("Debian packaging files [ok]generated[/]");

        Ok(())
    }

    fn configure(&self) -> Result<()> {
        info!(
            "[action]Configuring[/] sources/binaries for [pkg]{}[/]",
            self.cfg.name
        );

        let configure = match self.cfg.configure {
            Some(ref c) => c,
            None => {
                debug!("[skip]No configure block defined, [action]skipping[/][/]");
                return Ok(());
            }
        };

        if let Some(ref dirs) = configure.mkdir {
            self.do_mkdir(dirs)?;
        }
        if let Some(ref content) = configure.content {
            self.do_content(content)?;
        }
        if let Some(ref cp) = configure.cp {
            self.do_cp(cp)?;
        }
        if let Some(ref mv) = configure.mv {
            self.do_mv(mv)?;
        }
        if let Some(ref script) = configure.preinst {
            self.do_maint_script("preinst", script)?;
        }
        if let Some(ref script) = configure.prerm {
            self.do_maint_script("prerm", script)?;
        }
        if let Some(ref script) = configure.postinst {
            self.do_maint_script("postinst", script)?;
        }
        if let Some(ref script) = configure.postrm {
            self.do_maint_script("postrm", script)?;
        }

        Ok(())
    }

    fn do_mkdir(&self, directories: &[String]) -> Result<()> {
        for d in directories {
            let dir = self.buildroot.join(d);
            debug!("[action]Creating[/] directory [path]{}[/]", dir.display());
            fs::create_dir_all(&dir).context(format!(
                "Failed to create directory [path]{}[/]",
                dir.display()
            ))?;
        }
        Ok(())
    }

    fn do_cp(&self, files: &HashMap<String, String>) -> Result<()> {
        for (src, dst) in files {
            debug!("[action]Copying[/] [path]{}[/] to [path]{}[/]", src, dst);
            let status = Command::new("cp")
                .args(["-a", src, dst])
                .current_dir(&self.buildroot)
                .status()
                .context(format!(
                    "Failed to execute [cmd]cp[/] [path]{}[/] -> [path]{}[/]",
                    src, dst
                ))?;

            if !status.success() {
                crate::error_msg!(
                    "[cmd]cp[/] command failed for [path]{}[/] -> [path]{}[/] (status: {})",
                    src,
                    dst,
                    status
                );
            }
        }
        Ok(())
    }

    fn do_mv(&self, files: &HashMap<String, String>) -> Result<()> {
        for (src, dst) in files {
            debug!(
                "[action]Moving/renaming[/] [path]{}[/] to [path]{}[/]",
                src, dst
            );
            let status = Command::new("mv")
                .args([src.as_str(), dst.as_str()])
                .current_dir(&self.buildroot)
                .status()
                .context(format!(
                    "Failed to execute [cmd]mv[/] [path]{}[/] -> [path]{}[/]",
                    src, dst
                ))?;

            if !status.success() {
                crate::error_msg!(
                    "[cmd]mv[/] command failed for [path]{}[/] -> [path]{}[/] (status: {})",
                    src,
                    dst,
                    status
                );
            }
        }
        Ok(())
    }

    fn do_content(&self, files: &HashMap<String, String>) -> Result<()> {
        for (dst, data) in files {
            let dst_path = self.buildroot.join(dst);
            debug!(
                "[action]Adding[/] content to [path]{}[/]",
                dst_path.display()
            );
            self.write_file(&dst_path, data)?;
        }
        Ok(())
    }

    fn do_maint_script(&self, script: &str, content: &str) -> Result<()> {
        let dst_path = self.buildroot.join("debian").join(script);
        debug!(
            "[action]Adding[/] maintainer script [path]{}[/]",
            dst_path.display()
        );
        let full_content = format!(
            "#!/bin/sh\n\nset -e \n\n{}\n\n#DEBHELPER#\n\nexit 0",
            content
        );
        self.write_file(&dst_path, &full_content)
    }

    fn build_pkg(&self) -> Result<()> {
        info!(
            "[action]Building[/] Debian package for [pkg]{}[/]",
            self.cfg.name
        );

        fs::create_dir_all(&self.buildroot).context(format!(
            "Failed to create build root [path]{}[/]",
            self.buildroot.display()
        ))?;

        let cmd_args = [
            "debuild",
            "-i",
            "-us",
            "-uc",
            "--lintian-opts",
            "--color=always",
            "-IE",
            "--pedantic",
        ];

        info!("[action]Running[/] [cmd]{}[/]", cmd_args.join(" "));
        debug!("Working directory: [path]{}[/]", self.buildroot.display());

        let status = Command::new(cmd_args[0])
            .args(&cmd_args[1..])
            .current_dir(&self.buildroot)
            .env("DEB_BUILD_OPTIONS", "nostrip")
            .status()
            .context("Failed to execute [cmd]debuild[/]")?;

        if !status.success() {
            crate::error_msg!("[cmd]debuild[/] exited with status: {}", status);
        }

        if crate::logger::enabled_info() {
            self.show_package_contents()?;
        }

        Ok(())
    }

    fn show_package_contents(&self) -> Result<()> {
        let changes_pattern = format!("{}_*.changes", self.cfg.name);
        let mut found = false;

        for entry in fs::read_dir(&self.buildenv).context(format!(
            "Failed to read build environment [path]{}[/]",
            self.buildenv.display()
        ))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if glob_match(&name, &changes_pattern) {
                let changes_path = entry.path();
                info!(
                    "[action]Displaying[/] package contents (via [cmd]debc[/] [path]{}[/]):",
                    changes_path.display()
                );
                let status = Command::new("debc")
                    .arg(&changes_path)
                    .status()
                    .context("Failed to execute [cmd]debc[/] (is devscripts installed?)")?;

                if !status.success() {
                    warn!("debc exited with status: {}", status);
                }

                found = true;

                break;
            }
        }

        if !found {
            debug!("[skip]No .changes file found, [action]skipping[/] [cmd]debc[/][/]");
        }

        Ok(())
    }
}

/// Simple glob matching for `name_*.ext` patterns.
fn glob_match(name: &str, pattern: &str) -> bool {
    if let Some(star_pos) = pattern.find('*') {
        let prefix = &pattern[..star_pos];
        let suffix = &pattern[star_pos + 1..];
        name.starts_with(prefix) && name.ends_with(suffix)
    } else {
        name == pattern
    }
}

/// Parse a repo URL of the form `http://repo_url#dist#section#arch`.
/// Fragments are optional with defaults: dist=trixie, section=main, arch=amd64.
fn parse_repo_url(url: &str) -> (String, String, String, String) {
    let (repo_url, fragment) = match url.split_once('#') {
        Some((b, f)) => (b, f),
        None => (url, ""),
    };

    let fragments: Vec<&str> = if fragment.is_empty() {
        vec![]
    } else {
        fragment.split('#').collect()
    };

    let dist = fragments.first().unwrap_or(&"unstable").to_string();
    let dist = if dist.is_empty() {
        "unstable".to_string()
    } else {
        dist
    };

    let section = fragments.get(1).unwrap_or(&"main").to_string();
    let section = if section.is_empty() {
        "main".to_string()
    } else {
        section
    };

    let arch = fragments.get(2).unwrap_or(&"amd64").to_string();
    let arch = if arch.is_empty() {
        "amd64".to_string()
    } else {
        arch
    };

    (
        repo_url.trim_end_matches('/').to_string(),
        dist,
        section,
        arch,
    )
}

struct DputConfig {
    host: String,
    content: String,
}

/// Parse a dput URI of the form `method://login@fqdn/incoming?key=value&...`
/// and generate the corresponding dput.cf INI content.
fn generate_dput_cf(uri: &str) -> Result<DputConfig> {
    let (scheme, rest) = uri
        .split_once("://")
        .context("Invalid dput [url]URI[/]: missing method (expected method://...)")?;

    let method = scheme;
    let (authority_path, query) = match rest.split_once('?') {
        Some((ap, q)) => (ap, q),
        None => (rest, ""),
    };

    let (authority, incoming) = match authority_path.split_once('/') {
        Some((a, p)) => (a, format!("/{}", p)),
        None => (authority_path, "/incoming".to_string()),
    };

    let (login, fqdn) = match authority.split_once('@') {
        Some((l, f)) => (Some(l.to_string()), f.to_string()),
        None => (None, authority.to_string()),
    };

    let host_name = fqdn.replace('.', "-");

    let mut lines = vec![
        format!("[{}]", host_name),
        format!("fqdn = {}", fqdn),
        format!("method = {}", method),
        format!("incoming = {}", incoming),
    ];

    if let Some(ref login) = login {
        lines.push(format!("login = {}", login));
    }

    if !query.is_empty() {
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                lines.push(format!("{} = {}", key, value));
            }
        }
    }

    let content = lines.join("\n") + "\n";

    Ok(DputConfig {
        host: host_name,
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_supports_simple_star_pattern() {
        assert!(glob_match("mypkg_1.2.3_amd64.changes", "mypkg_*.changes"));
        assert!(!glob_match("other_1.2.3_amd64.changes", "mypkg_*.changes"));
    }

    #[test]
    fn parse_repo_url_uses_defaults_for_missing_fragments() {
        let (repo, dist, section, arch) = parse_repo_url("https://repo.example.org/debian/");
        assert_eq!(repo, "https://repo.example.org/debian");
        assert_eq!(dist, "unstable");
        assert_eq!(section, "main");
        assert_eq!(arch, "amd64");
    }

    #[test]
    fn parse_repo_url_accepts_partial_and_empty_fragments() {
        let (repo, dist, section, arch) =
            parse_repo_url("https://repo.example.org/debian#bookworm##arm64");
        assert_eq!(repo, "https://repo.example.org/debian");
        assert_eq!(dist, "bookworm");
        assert_eq!(section, "main");
        assert_eq!(arch, "arm64");
    }

    #[test]
    fn generate_dput_cf_parses_full_uri() {
        let cfg = generate_dput_cf(
            "scp://deploy@repo.example.com/var/spool/incoming?hash=md5&allow_unsigned_uploads=1",
        )
        .expect("valid dput URI");

        assert_eq!(cfg.host, "repo-example-com");
        assert!(cfg.content.contains("[repo-example-com]"));
        assert!(cfg.content.contains("method = scp"));
        assert!(cfg.content.contains("fqdn = repo.example.com"));
        assert!(cfg.content.contains("login = deploy"));
        assert!(cfg.content.contains("incoming = /var/spool/incoming"));
        assert!(cfg.content.contains("hash = md5"));
        assert!(cfg.content.contains("allow_unsigned_uploads = 1"));
    }

    #[test]
    fn generate_dput_cf_uses_default_incoming_path() {
        let cfg = generate_dput_cf("scp://repo.example.com").expect("valid dput URI");
        assert!(cfg.content.contains("incoming = /incoming"));
    }
}
