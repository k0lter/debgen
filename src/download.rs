use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use regex::Regex;
use reqwest::Url;
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder};
use serde::Deserialize;

use tracing::{debug, info, trace, warn};

#[derive(Debug, Clone, Default)]
pub struct AuthTokens {
    pub github: Option<String>,
    pub gitlab: Option<GitLabAuth>,
}

/// GitLab authentication scheme. GitLab does not accept a CI job token via
/// `Authorization: Bearer` nor `PRIVATE-TOKEN`; it must be sent as `JOB-TOKEN`.
#[derive(Debug, Clone)]
pub enum GitLabAuth {
    /// Personal/project/group access token, sent as `Authorization: Bearer`.
    PrivateToken(String),
    /// CI/CD job token (`CI_JOB_TOKEN`), sent as `JOB-TOKEN`.
    JobToken(String),
}

impl GitLabAuth {
    /// Apply the appropriate GitLab authentication header to a request.
    pub fn apply(&self, req: RequestBuilder) -> RequestBuilder {
        match self {
            GitLabAuth::PrivateToken(t) => req.header("Authorization", format!("Bearer {}", t)),
            GitLabAuth::JobToken(t) => req.header("JOB-TOKEN", t),
        }
    }
}

#[derive(Clone, Copy)]
enum Auth<'a> {
    None,
    Bearer(&'a str),
    GitLab(&'a GitLabAuth),
}

impl Auth<'_> {
    fn apply(&self, req: RequestBuilder) -> RequestBuilder {
        match self {
            Auth::None => req,
            Auth::Bearer(t) => req.header("Authorization", format!("Bearer {}", t)),
            Auth::GitLab(a) => a.apply(req),
        }
    }

    fn is_some(&self) -> bool {
        !matches!(self, Auth::None)
    }
}

#[derive(Debug, Clone)]
pub enum ParsedUrl {
    GitHub(String),
    GitLab { host: String, project: String },
    Http(String),
    File(PathBuf),
}

const GITLAB_DEFAULT_HOST: &str = "gitlab.com";

/// Parse a location URL string into a typed variant.
/// GitHub:  `github://owner/repo`
/// GitLab:  `gitlab://group/repo` (uses gitlab.com)
///          `gitlab://host.example.com/group/repo` (self-hosted)
/// File:    `file:///absolute/path`
/// HTTP:    `http(s)://...`
pub fn parse_download_url(raw: &str) -> Result<ParsedUrl> {
    let trimmed = raw.trim();

    if let Some(rest) = trimmed.strip_prefix("github://") {
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
            crate::error_msg!(
                "Invalid [cmd]github://[/] URL: expected [cmd]github://owner/repo[/], got: [path]{}[/]",
                trimmed
            );
        }
        let id = format!("{}/{}", parts[0], parts[1]);
        return Ok(ParsedUrl::GitHub(id));
    }

    if let Some(rest) = trimmed.strip_prefix("gitlab://") {
        if rest.is_empty() || !rest.contains('/') {
            crate::error_msg!(
                "Invalid [cmd]gitlab://[/] URL: expected [cmd]gitlab://[host/]group/repo[/], got: [path]{}[/]",
                trimmed
            );
        }
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        let (host, project) = if parts[0].contains('.') {
            // First segment looks like a hostname (contains a dot)
            let remainder = parts[1];
            if remainder.is_empty() || !remainder.contains('/') {
                crate::error_msg!(
                    "Invalid [cmd]gitlab://[/] URL: expected [cmd]gitlab://host/group/repo[/], got: [path]{}[/]",
                    trimmed
                );
            }
            (parts[0].to_string(), remainder.to_string())
        } else {
            (GITLAB_DEFAULT_HOST.to_string(), rest.to_string())
        };
        return Ok(ParsedUrl::GitLab { host, project });
    }

    if let Some(rest) = trimmed.strip_prefix("file://") {
        let path = PathBuf::from(rest);
        if !path.exists() {
            crate::error_msg!(
                "[cmd]file://[/] path does not exist: [path]{}[/]",
                path.display()
            );
        }
        if !path.is_dir() {
            crate::error_msg!(
                "[cmd]file://[/] path is not a directory: [path]{}[/]",
                path.display()
            );
        }
        return Ok(ParsedUrl::File(path));
    }

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Ok(ParsedUrl::Http(trimmed.to_string()));
    }

    crate::error_msg!(
        "Unsupported URL scheme: [field]{}[/]. Supported: [cmd]github://[/], [cmd]gitlab://[/], [cmd]file://[/], [cmd]http://[/], [cmd]https://[/]",
        trimmed
    );
}

fn build_http_client(user_agent: &str) -> Result<Client> {
    ClientBuilder::new()
        .user_agent(user_agent)
        .build()
        .context("Failed to build HTTP client")
}

fn auth_get(client: &Client, url: &str, auth: Auth) -> RequestBuilder {
    let req = client.get(url);
    if auth.is_some() {
        debug!("[action]Using authentication token[/]");
    }
    auth.apply(req)
}

/// Download a file from a URL to a destination path, with streaming.
fn download_file(client: &Client, url: &str, dest: &Path, auth: Auth) -> Result<()> {
    info!("[action]Downloading[/] [url]{}[/]", url);
    debug!("Download destination: [path]{}[/]", dest.display());

    let mut response = auth_get(client, url, auth)
        .send()
        .context(format!("Failed to send request to [url]{}[/]", url))?
        .error_for_status()
        .context(format!("HTTP error for [url]{}[/]", url))?;

    let mut file = fs::File::create(dest)
        .context(format!("Failed to create file [path]{}[/]", dest.display()))?;

    let mut buf = [0u8; 8192];
    loop {
        let n = response
            .read(&mut buf)
            .context(format!("Failed to read response body from [url]{}[/]", url))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .context(format!("Failed to write to [path]{}[/]", dest.display()))?;
    }

    let size = fs::metadata(dest)
        .context(format!(
            "Failed to stat downloaded file [path]{}[/]",
            dest.display()
        ))?
        .len();

    if size == 0 {
        crate::error_msg!("Downloaded file is empty: [path]{}[/]", dest.display());
    }

    debug!(
        "Downloaded [value]{}[/] bytes to [path]{}[/]",
        size,
        dest.display()
    );

    Ok(())
}

// ---------- System tool helpers ----------

fn require_command(name: &str) -> Result<()> {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .context(format!(
            "[cmd]{}[/] is not installed. Please install it and try again.",
            name
        ))?;
    Ok(())
}

fn run_extraction(cmd: &str, args: &[&str], cwd: &Path, description: &str) -> Result<()> {
    debug!(
        "[action]Running[/] [cmd]{}[/] {} (cwd: [path]{}[/])",
        cmd,
        args.join(" "),
        cwd.display()
    );

    let status = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .status()
        .context(format!("Failed to execute [cmd]{}[/]", cmd))?;

    if !status.success() {
        crate::error_msg!("[cmd]{}[/] failed (status: {})", description, status);
    }

    Ok(())
}

/// Extract an archive using system tools.
fn extract_archive(file_path: &Path, target_dir: &Path) -> Result<bool> {
    let filename = file_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    let abs_archive = file_path.canonicalize().context(format!(
        "Failed to resolve archive path [path]{}[/]",
        file_path.display()
    ))?;
    let archive_str = abs_archive.to_string_lossy();

    debug!(
        "[action]Attempting archive extraction[/]: [path]{}[/] -> [path]{}[/]",
        file_path.display(),
        target_dir.display()
    );

    if filename.ends_with(".tar.gz") || filename.ends_with(".tgz") {
        info!(
            "[action]Extracting[/] tar.gz archive: [path]{}[/]",
            filename
        );
        require_command("tar")?;
        run_extraction(
            "tar",
            &["xzf", &archive_str],
            target_dir,
            &format!("tar.gz extraction of {}", filename),
        )?;
        fs::remove_file(file_path).context(format!(
            "Failed to remove archive [path]{}[/]",
            file_path.display()
        ))?;
        debug!("Extracted tar.gz to [path]{}[/]", target_dir.display());
        return Ok(true);
    }

    if filename.ends_with(".tar.xz") || filename.ends_with(".txz") {
        info!(
            "[action]Extracting[/] tar.xz archive: [path]{}[/]",
            filename
        );
        require_command("tar")?;
        require_command("xz")?;
        run_extraction(
            "tar",
            &["xJf", &archive_str],
            target_dir,
            &format!("tar.xz extraction of {}", filename),
        )?;
        fs::remove_file(file_path).context(format!(
            "Failed to remove archive [path]{}[/]",
            file_path.display()
        ))?;
        debug!("Extracted tar.xz to [path]{}[/]", target_dir.display());
        return Ok(true);
    }

    if filename.ends_with(".tar.bz2") || filename.ends_with(".tbz2") {
        info!(
            "[action]Extracting[/] tar.bz2 archive: [path]{}[/]",
            filename
        );
        require_command("tar")?;
        require_command("bzip2")?;
        run_extraction(
            "tar",
            &["xjf", &archive_str],
            target_dir,
            &format!("tar.bz2 extraction of {}", filename),
        )?;
        fs::remove_file(file_path).context(format!(
            "Failed to remove archive [path]{}[/]",
            file_path.display()
        ))?;
        debug!("Extracted tar.bz2 to [path]{}[/]", target_dir.display());
        return Ok(true);
    }

    if filename.ends_with(".tar") {
        info!("[action]Extracting[/] tar archive: [path]{}[/]", filename);
        require_command("tar")?;
        run_extraction(
            "tar",
            &["xf", &archive_str],
            target_dir,
            &format!("tar extraction of {}", filename),
        )?;
        fs::remove_file(file_path).context(format!(
            "Failed to remove archive [path]{}[/]",
            file_path.display()
        ))?;
        debug!("Extracted tar to [path]{}[/]", target_dir.display());
        return Ok(true);
    }

    if filename.ends_with(".gz") {
        info!(
            "[action]Extracting[/] gzip compressed file: [path]{}[/]",
            filename
        );
        require_command("gunzip")?;
        run_extraction(
            "gunzip",
            &["-f", &archive_str],
            target_dir,
            &format!("gzip extraction of {}", filename),
        )?;
        debug!("Extracted gzip to [path]{}[/]", target_dir.display());
        return Ok(true);
    }

    if filename.ends_with(".zip") {
        info!("[action]Extracting[/] zip archive: [path]{}[/]", filename);
        require_command("unzip")?;
        run_extraction(
            "unzip",
            &["-o", &archive_str, "-d", &target_dir.to_string_lossy()],
            target_dir,
            &format!("zip extraction of {}", filename),
        )?;
        fs::remove_file(file_path).context(format!(
            "Failed to remove archive [path]{}[/]",
            file_path.display()
        ))?;
        debug!("Extracted zip to [path]{}[/]", target_dir.display());
        return Ok(true);
    }

    debug!(
        "No recognized archive format for [path]{}[/], [action]skipping extraction[/]",
        filename
    );

    Ok(false)
}

/// After extraction, if the temp dir contains a single subdirectory and no files,
/// return that subdirectory as the effective root.
fn resolve_extract_root(dir: &Path) -> Result<PathBuf> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in fs::read_dir(dir).context(format!(
        "Failed to read directory [path]{}[/]",
        dir.display()
    ))? {
        let entry = entry.context("Failed to read directory entry")?;
        let ft = entry.file_type().context(format!(
            "Failed to get file type for [path]{:?}[/]",
            entry.path()
        ))?;
        if ft.is_dir() {
            dirs.push(entry.path());
        } else if ft.is_file() {
            files.push(entry.path());
        }
    }

    if dirs.len() == 1 && files.is_empty() {
        let root = dirs.into_iter().next().unwrap();
        debug!(
            "Resolved single subdirectory as extract root: [path]{}[/]",
            root.display()
        );
        return Ok(root);
    }

    debug!(
        "Extract root is directory itself: [path]{}[/] ([value]{}[/] dirs, [value]{}[/] files)",
        dir.display(),
        dirs.len(),
        files.len()
    );

    Ok(dir.to_path_buf())
}

/// Move all contents from `src_dir` into `dst_dir`.
fn move_contents(src_dir: &Path, dst_dir: &Path) -> Result<()> {
    debug!(
        "[action]Moving contents[/] from [path]{}[/] to [path]{}[/]",
        src_dir.display(),
        dst_dir.display()
    );

    for entry in fs::read_dir(src_dir).context(format!(
        "Failed to read source directory [path]{}[/]",
        src_dir.display()
    ))? {
        let entry = entry.context("Failed to read directory entry during move")?;
        let src = entry.path();
        let name = entry.file_name();
        let dst = dst_dir.join(&name);

        if dst.exists() && dst.is_dir() {
            fs::remove_dir_all(&dst).context(format!(
                "Failed to remove existing directory [path]{}[/]",
                dst.display()
            ))?;
        }

        fs::rename(&src, &dst).or_else(|_| -> anyhow::Result<()> {
            if src.is_dir() {
                copy_dir_recursive(&src, &dst)?;
                fs::remove_dir_all(&src).context(format!(
                    "Failed to remove source directory [path]{}[/]",
                    src.display()
                ))?;
            } else {
                fs::copy(&src, &dst).context(format!(
                    "Failed to copy [path]{}[/] to [path]{}[/]",
                    src.display(),
                    dst.display()
                ))?;
                fs::remove_file(&src).context(format!(
                    "Failed to remove source file [path]{}[/]",
                    src.display()
                ))?;
            }
            Ok(())
        })?;
    }

    debug!("Moved all contents to [path]{}[/]", dst_dir.display());

    Ok(())
}

pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).context(format!(
        "Failed to create directory [path]{}[/]",
        dst.display()
    ))?;

    for entry in fs::read_dir(src).context(format!(
        "Failed to read directory [path]{}[/]",
        src.display()
    ))? {
        let entry = entry.context("Failed to read directory entry during copy")?;
        let ty = entry.file_type().context(format!(
            "Failed to get file type for [path]{:?}[/]",
            entry.path()
        ))?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).context(format!(
                "Failed to copy [path]{}[/] to [path]{}[/]",
                entry.path().display(),
                target.display()
            ))?;
        }
    }

    Ok(())
}

/// Decompress data piped through a system command (xz, gunzip, etc.).
pub fn pipe_decompress(cmd: &str, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    require_command(cmd)?;

    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(format!("Failed to spawn [cmd]{}[/]", cmd))?;

    let mut stdin = child.stdin.take().context("Failed to open stdin pipe")?;
    let mut stdout = child.stdout.take().context("Failed to open stdout pipe")?;

    let input_owned = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input_owned);
    });

    let mut decompressed = Vec::new();
    stdout.read_to_end(&mut decompressed).context(format!(
        "Failed to read decompressed output from [cmd]{}[/]",
        cmd
    ))?;

    writer.join().ok();

    let status = child
        .wait()
        .context(format!("Failed to wait for [cmd]{}[/]", cmd))?;
    if !status.success() {
        crate::error_msg!("[cmd]{}[/] decompression failed (status: {})", cmd, status);
    }

    Ok(decompressed)
}

#[derive(Debug)]
pub struct DownloadResult {
    pub extract_path: PathBuf,
    pub version: Option<String>,
}

fn extract_version_from_flavor_capture(flavor: &Regex, candidate: &str) -> Option<String> {
    let captures = flavor.captures(candidate)?;
    captures.name("version").map(|m| m.as_str().to_string())
}

// ---------- GitHub ----------

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// Compile a flavor string into a regex.
/// The flavor is treated as a regex pattern; if it is invalid, it is escaped
/// and used as a literal substring match.
fn compile_flavor(flavor: &str) -> Regex {
    match Regex::new(flavor) {
        Ok(re) => {
            debug!("Compiled flavor as regex: [field]{}[/]", flavor);
            re
        }
        Err(err) => {
            warn!(
                "Flavor [field]{}[/] is not a valid regex ([field]{}[/]), treating as literal",
                flavor, err
            );
            Regex::new(&regex::escape(flavor)).unwrap()
        }
    }
}

/// Extract a version string from a tag name.
/// Handles formats like `v1.2.3`, `project-v1.2.3`, `project-1.2.3`.
fn extract_version_from_tag(tag: &str) -> String {
    let date_re = Regex::new(r"(?P<date>\d{4}-\d{2}-\d{2})$").unwrap();
    if let Some(caps) = date_re.captures(tag) {
        let date = caps.name("date").unwrap().as_str();
        return date.replace('-', ".");
    }

    let common_re = Regex::new(r"^(?:.+-)?v?(?P<version>.+)$").unwrap();
    if let Some(caps) = common_re.captures(tag) {
        return caps.name("version").unwrap().as_str().to_string();
    }

    tag.to_string()
}

pub fn github_download(
    project: &str,
    flavor: &Regex,
    work_dir: &Path,
    token: Option<&str>,
) -> Result<DownloadResult> {
    info!(
        "[action]Looking for[/] GitHub release of [pkg]{}[/] (flavor: [field]{}[/])",
        project, flavor
    );
    let auth = token.map_or(Auth::None, Auth::Bearer);
    let client = build_http_client("GitHub-Release-Downloader/1.0")?;

    let api_url = format!("https://api.github.com/repos/{}/releases/latest", project);
    debug!(
        "[action]Fetching[/] latest release from [url]{}[/]",
        api_url
    );

    let release: GhRelease = auth_get(&client, &api_url, auth)
        .send()
        .context(format!(
            "Failed to reach GitHub API for [pkg]{}[/]",
            project
        ))?
        .error_for_status()
        .context(format!(
            "Failed to fetch latest release for [pkg]{}[/]",
            project
        ))?
        .json()
        .context("Failed to parse GitHub API [field]response[/]")?;
    debug!(
        "GitHub latest release [version]{}[/] has [value]{}[/] assets",
        release.tag_name,
        release.assets.len()
    );
    if release.assets.is_empty() {
        debug!(
            "GitHub release [version]{}[/] has no assets to evaluate against flavor [field]{}[/]",
            release.tag_name, flavor
        );
    }

    let mut picked = None;
    let mut matched_version = None;
    for a in &release.assets {
        trace!(
            "Evaluating GitHub asset candidate [path]{}[/] against flavor [field]{}[/]",
            a.name, flavor
        );
        if flavor.is_match(&a.name) {
            matched_version = extract_version_from_flavor_capture(flavor, &a.name);
            picked = Some(a);
            break;
        }
        trace!(
            "GitHub asset did not match flavor [field]{}[/]: [path]{}[/]",
            flavor, a.name
        );
    }
    let asset = picked.context(format!(
        "No asset matching flavor [field]{}[/] in release [version]{}[/] of [pkg]{}[/]",
        flavor, release.tag_name, project
    ))?;

    let version = matched_version.unwrap_or_else(|| extract_version_from_tag(&release.tag_name));
    info!(
        "Found version [version]{}[/] for [pkg]{}[/]",
        version, project
    );
    debug!("Selected asset: [path]{}[/]", asset.name);

    let filename = asset
        .browser_download_url
        .rsplit('/')
        .next()
        .unwrap_or("download");
    let file_path = work_dir.join(filename);

    download_file(&client, &asset.browser_download_url, &file_path, auth)?;
    extract_archive(&file_path, work_dir)?;

    let extract_path = resolve_extract_root(work_dir)?;
    debug!(
        "GitHub download extracted to: [path]{}[/]",
        extract_path.display()
    );

    Ok(DownloadResult {
        extract_path,
        version: Some(version),
    })
}

// ---------- GitLab ----------

#[derive(Deserialize)]
struct GlRelease {
    tag_name: String,
    assets: GlAssets,
}

#[derive(Deserialize)]
struct GlAssets {
    #[serde(default)]
    links: Vec<GlLink>,
}

#[derive(Deserialize)]
struct GlLink {
    name: String,
    url: String,
}

pub fn gitlab_download(
    host: &str,
    project: &str,
    flavor: &Regex,
    work_dir: &Path,
    token: Option<&GitLabAuth>,
) -> Result<DownloadResult> {
    info!(
        "[action]Looking for[/] GitLab release of [pkg]{}[/] on [url]{}[/] (flavor: [field]{}[/])",
        project, host, flavor
    );
    let auth = token.map_or(Auth::None, Auth::GitLab);
    let client = build_http_client("GitLab-Release-Downloader/1.0")?;

    let encoded_project = project.replace('/', "%2F");
    let api_url = format!(
        "https://{}/api/v4/projects/{}/releases",
        host, encoded_project
    );
    debug!("[action]Fetching[/] release info from [url]{}[/]", api_url);

    let releases: Vec<GlRelease> = auth_get(&client, &api_url, auth)
        .send()
        .context(format!(
            "Failed to reach GitLab API at [url]{}[/] for [pkg]{}[/]",
            host, project
        ))?
        .error_for_status()
        .context(format!(
            "Failed to fetch releases for [pkg]{}[/] on [url]{}[/]",
            project, host
        ))?
        .json()
        .context("Failed to parse GitLab API [field]response[/]")?;
    debug!(
        "GitLab API returned [value]{}[/] releases for [pkg]{}[/]",
        releases.len(),
        project
    );

    if releases.is_empty() {
        crate::error_msg!(
            "No releases found for [pkg]{}[/] on [url]{}[/]",
            project,
            host
        );
    }

    let mut picked = None;
    'outer: for r in &releases {
        debug!(
            "Evaluating GitLab release [version]{}[/] with [value]{}[/] links",
            r.tag_name,
            r.assets.links.len()
        );
        if r.assets.links.is_empty() {
            debug!(
                "GitLab release [version]{}[/] has no links to evaluate against flavor [field]{}[/]",
                r.tag_name, flavor
            );
        }
        for l in &r.assets.links {
            trace!(
                "Evaluating GitLab link candidate [path]{}[/] ([url]{}[/]) against flavor [field]{}[/]",
                l.name, l.url, flavor
            );
            if flavor.is_match(&l.url) {
                let captured = extract_version_from_flavor_capture(flavor, &l.name)
                    .or_else(|| {
                        basename_from_url(&l.url).and_then(|basename| {
                            extract_version_from_flavor_capture(flavor, &basename)
                        })
                    })
                    .or_else(|| extract_version_from_flavor_capture(flavor, &l.url));
                picked = Some((
                    r.tag_name.as_str(),
                    l.url.as_str(),
                    l.name.as_str(),
                    captured,
                ));
                break 'outer;
            }
            trace!(
                "GitLab link did not match flavor [field]{}[/]: [path]{}[/] ([url]{}[/])",
                flavor, l.name, l.url
            );
        }
    }

    let (tag_name, download_url, asset_label, matched_version) = picked.context(format!(
        "No asset matching flavor [field]{}[/] in any release of [pkg]{}[/] on [url]{}[/]",
        flavor, project, host
    ))?;

    let version = matched_version.unwrap_or_else(|| extract_version_from_tag(tag_name));
    info!(
        "Found version [version]{}[/] for [pkg]{}[/]",
        version, project
    );
    debug!(
        "Selected asset: [path]{}[/] (release [version]{}[/])",
        asset_label, tag_name
    );

    let filename = download_url.rsplit('/').next().unwrap_or("download");
    let file_path = work_dir.join(filename);

    download_file(&client, download_url, &file_path, auth)?;
    extract_archive(&file_path, work_dir)?;

    let extract_path = resolve_extract_root(work_dir)?;
    debug!(
        "GitLab download extracted to: [path]{}[/]",
        extract_path.display()
    );

    Ok(DownloadResult {
        extract_path,
        version: Some(version),
    })
}

// ---------- HTTP(S) ----------

fn basename_from_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    parsed
        .path_segments()?
        .rfind(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
}

fn resolve_http_link(base_url: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }

    if let Ok(abs) = Url::parse(href) {
        if abs.scheme() == "http" || abs.scheme() == "https" {
            return Some(abs.to_string());
        }
        return None;
    }

    let base = Url::parse(base_url).ok()?;
    let joined = base.join(href).ok()?;
    if joined.scheme() == "http" || joined.scheme() == "https" {
        return Some(joined.to_string());
    }

    None
}

fn extract_html_links(body: &str) -> Vec<String> {
    // ponytail: Regex-based extraction handles common href forms; if pages become malformed/JS-driven, switch to an HTML parser crate.
    let mut links = Vec::new();
    let href_re = Regex::new(r#"(?is)<a\b[^>]*?\bhref\s*=\s*["']([^"']+)["']"#).unwrap();
    for caps in href_re.captures_iter(body) {
        if let Some(m) = caps.get(1) {
            links.push(m.as_str().to_string());
        }
    }
    links
}

fn extract_text_links(body: &str) -> Vec<String> {
    let mut links = Vec::new();
    let absolute_re = Regex::new(r#"https?://[^\s"'<>]+"#).unwrap();

    for caps in absolute_re.captures_iter(body) {
        if let Some(m) = caps.get(0) {
            links.push(m.as_str().to_string());
        }
    }

    for line in body.lines() {
        let candidate = line.trim();
        if candidate.is_empty() || candidate.contains(' ') {
            continue;
        }
        links.push(candidate.to_string());
    }

    links
}

fn extract_listing_links(content_type: &str, body: &str) -> Option<Vec<String>> {
    if content_type.starts_with("text/html") {
        return Some(extract_html_links(body));
    }
    if content_type.starts_with("text/plain") {
        return Some(extract_text_links(body));
    }
    None
}

fn pick_best_matching_link(
    base_url: &str,
    raw_links: Vec<String>,
    flavor: &Regex,
) -> Option<(String, Option<String>)> {
    let mut seen = HashSet::new();
    let mut matches: Vec<(String, String, Option<String>)> = raw_links
        .into_iter()
        .filter_map(|href| resolve_http_link(base_url, &href))
        .filter_map(|resolved| {
            let basename = basename_from_url(&resolved)?;
            if !flavor.is_match(&basename) {
                return None;
            }
            if !seen.insert(resolved.clone()) {
                return None;
            }
            let captured = extract_version_from_flavor_capture(flavor, &basename);
            Some((basename, resolved, captured))
        })
        .collect();

    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    matches
        .into_iter()
        .next()
        .map(|(_, url, version)| (url, version))
}

fn resolve_http_archive_url(
    client: &Client,
    source_url: &str,
    flavor: Option<&Regex>,
) -> Result<(String, Option<String>)> {
    let response = client
        .get(source_url)
        .send()
        .context(format!(
            "Failed to inspect HTTP source [url]{}[/] before download",
            source_url
        ))?
        .error_for_status()
        .context(format!(
            "HTTP error while inspecting source [url]{}[/]",
            source_url
        ))?;

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    if !(content_type.starts_with("text/html") || content_type.starts_with("text/plain")) {
        debug!(
            "HTTP source is not a listing ([field]content-type[/]=[field]{}[/]), using URL directly",
            content_type
        );
        let matched_version = flavor.and_then(|re| {
            basename_from_url(source_url)
                .and_then(|basename| extract_version_from_flavor_capture(re, &basename))
        });
        return Ok((source_url.to_string(), matched_version));
    }

    let flavor = flavor.context(
        "[field]flavor[/] is required when HTTP location points to an HTML/TXT listing (use 'flavor' field in YAML or --flavor argument)",
    )?;
    let body = response.text().context(format!(
        "Failed to read listing body from [url]{}[/]",
        source_url
    ))?;

    let raw_links = extract_listing_links(&content_type, &body).context(format!(
        "No listing parser available for [field]content-type[/] [field]{}[/] at [url]{}[/]",
        content_type, source_url
    ))?;

    if let Some((selected, matched_version)) =
        pick_best_matching_link(source_url, raw_links.clone(), flavor)
    {
        info!(
            "[action]Resolved[/] archive URL from listing: [url]{}[/]",
            selected
        );
        return Ok((selected, matched_version));
    }

    // ponytail: one-level listing crawl keeps behavior simple; if indexes become deeper, switch to bounded BFS depth.
    let mut listing_candidates: Vec<String> = raw_links
        .into_iter()
        .filter_map(|href| resolve_http_link(source_url, &href))
        .filter(|candidate| candidate.ends_with('/'))
        .collect();
    listing_candidates.sort();
    listing_candidates.dedup();

    for listing_url in listing_candidates {
        debug!(
            "[action]Inspecting nested listing[/] [url]{}[/] for flavor [field]{}[/]",
            listing_url, flavor
        );
        let nested_response = client
            .get(&listing_url)
            .send()
            .context(format!(
                "Failed to inspect nested listing [url]{}[/]",
                listing_url
            ))?
            .error_for_status()
            .context(format!(
                "HTTP error while inspecting nested listing [url]{}[/]",
                listing_url
            ))?;

        let nested_content_type = nested_response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        let nested_body = nested_response.text().context(format!(
            "Failed to read nested listing body from [url]{}[/]",
            listing_url
        ))?;

        if let Some((selected, matched_version)) = pick_best_matching_link(
            &listing_url,
            extract_listing_links(&nested_content_type, &nested_body).unwrap_or_default(),
            flavor,
        ) {
            info!(
                "[action]Resolved[/] archive URL from nested listing: [url]{}[/]",
                selected
            );
            return Ok((selected, matched_version));
        }
    }

    crate::error_msg!(
        "No listing link matched flavor [field]{}[/] from [url]{}[/] (including nested listings)",
        flavor,
        source_url
    )
}

pub fn http_download(url: &str, work_dir: &Path, flavor: Option<&Regex>) -> Result<DownloadResult> {
    info!("[action]Downloading[/] from HTTP(S): [url]{}[/]", url);
    let client = build_http_client("Debgen-Downloader/1.0")?;

    let (archive_url, matched_version) = resolve_http_archive_url(&client, url, flavor)?;
    let filename = basename_from_url(&archive_url).unwrap_or_else(|| "download".to_string());
    let file_path = work_dir.join(filename);

    download_file(&client, &archive_url, &file_path, Auth::None)?;
    extract_archive(&file_path, work_dir)?;

    let extract_path = resolve_extract_root(work_dir)?;
    debug!(
        "HTTP download extracted to: [path]{}[/]",
        extract_path.display()
    );

    Ok(DownloadResult {
        extract_path,
        version: matched_version,
    })
}

// ---------- file:// ----------

pub fn file_copy(source: &Path, work_dir: &Path) -> Result<DownloadResult> {
    info!(
        "[action]Copying[/] local source from [path]{}[/] to [path]{}[/]",
        source.display(),
        work_dir.display()
    );

    let source = source.canonicalize().context(format!(
        "Failed to resolve source path [path]{}[/]",
        source.display()
    ))?;

    if !source.is_dir() {
        crate::error_msg!(
            "[cmd]file://[/] source is not a directory: [path]{}[/]",
            source.display()
        );
    }

    copy_dir_recursive(&source, work_dir).context(format!(
        "Failed to copy source directory [path]{}[/]",
        source.display()
    ))?;

    debug!("Copied local source to [path]{}[/]", work_dir.display());

    Ok(DownloadResult {
        extract_path: work_dir.to_path_buf(),
        version: None,
    })
}

/// High-level: perform download according to parsed URL, extract into `build_root`.
/// `flavor` is required for GitHub/GitLab to select the right release asset.
pub fn perform_download(
    parsed: &ParsedUrl,
    build_root: &Path,
    flavor: Option<&str>,
    tokens: &AuthTokens,
) -> Result<DownloadResult> {
    debug!(
        "[action]Performing download[/] for {:?} into [path]{}[/]",
        parsed,
        build_root.display()
    );

    match parsed {
        ParsedUrl::File(source) => file_copy(source, build_root),
        ParsedUrl::Http(url) => {
            let tmp =
                tempfile::tempdir().context("Failed to create temporary [path]directory[/]")?;
            let tmp_path = tmp.path();
            debug!(
                "[action]Using[/] temporary directory: [path]{}[/]",
                tmp_path.display()
            );

            let flavor_re = flavor.map(compile_flavor);
            let result = http_download(url, tmp_path, flavor_re.as_ref())?;
            move_contents(&result.extract_path, build_root)?;
            finalize_download(build_root, result)
        }
        ParsedUrl::GitHub(project) => {
            let flavor_str = flavor.context(
                "[field]flavor[/] is required for [url]github://[/] locations (use 'flavor' field in YAML or --flavor argument)",
            )?;
            let flavor_re = compile_flavor(flavor_str);
            let tmp =
                tempfile::tempdir().context("Failed to create temporary [path]directory[/]")?;
            let tmp_path = tmp.path();
            debug!(
                "[action]Using[/] temporary directory: [path]{}[/]",
                tmp_path.display()
            );

            let result = github_download(project, &flavor_re, tmp_path, tokens.github.as_deref())?;
            move_contents(&result.extract_path, build_root)?;
            finalize_download(build_root, result)
        }
        ParsedUrl::GitLab { host, project } => {
            let flavor_str = flavor.context(
                "[field]flavor[/] is required for [url]gitlab://[/] locations (use 'flavor' field in YAML or --flavor argument)",
            )?;
            let flavor_re = compile_flavor(flavor_str);
            let tmp =
                tempfile::tempdir().context("Failed to create temporary [path]directory[/]")?;
            let tmp_path = tmp.path();
            debug!(
                "[action]Using[/] temporary directory: [path]{}[/]",
                tmp_path.display()
            );

            let result =
                gitlab_download(host, project, &flavor_re, tmp_path, tokens.gitlab.as_ref())?;
            move_contents(&result.extract_path, build_root)?;
            finalize_download(build_root, result)
        }
    }
}

fn finalize_download(build_root: &Path, result: DownloadResult) -> Result<DownloadResult> {
    debug!(
        "Download complete, sources available at [path]{}[/]",
        build_root.display()
    );

    Ok(DownloadResult {
        extract_path: build_root.to_path_buf(),
        version: result.version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_archive_from_html_listing_prefers_descending_basename() {
        let html = r#"
        <html>
          <body>
            <a href="pkg-v1.2.0-linux-amd64.tar.gz">old</a>
            <a href="/releases/pkg-v1.3.0-linux-amd64.tar.gz">new</a>
            <a href="https://cdn.example.org/pkg-v1.1.0-linux-amd64.tar.gz">older</a>
          </body>
        </html>
        "#;
        let flavor = Regex::new(r"linux-amd64\.tar\.gz$").unwrap();
        let selected = pick_best_matching_link(
            "https://example.org/downloads/",
            extract_listing_links("text/html", html).unwrap_or_default(),
            &flavor,
        );

        assert_eq!(
            selected.map(|(url, _)| url),
            Some("https://example.org/releases/pkg-v1.3.0-linux-amd64.tar.gz".to_string())
        );
    }

    #[test]
    fn select_archive_from_text_listing_with_relative_links() {
        let text = "pkg-v1.9.0.tar.gz\npkg-v1.10.0.tar.gz\n";
        let flavor = Regex::new(r"^pkg-v1\.\d+\.\d+\.tar\.gz$").unwrap();
        let selected = pick_best_matching_link(
            "https://downloads.example.com/project/",
            extract_listing_links("text/plain; charset=utf-8", text).unwrap_or_default(),
            &flavor,
        );

        assert_eq!(
            selected.map(|(url, _)| url),
            Some("https://downloads.example.com/project/pkg-v1.9.0.tar.gz".to_string())
        );
    }

    #[test]
    fn pick_best_matching_link_supports_nested_listing_resolution_input() {
        let links = vec![
            "../".to_string(),
            "archives/".to_string(),
            "build.sh".to_string(),
            "https://cdn.example.org/geoip-db_1.0.20260618.tar.xz".to_string(),
        ];
        let flavor = Regex::new(r"geoip-db_[\.\d]+.tar").unwrap();
        let selected = pick_best_matching_link("https://geo.example.org", links, &flavor);

        assert_eq!(
            selected.map(|(url, _)| url),
            Some("https://cdn.example.org/geoip-db_1.0.20260618.tar.xz".to_string())
        );
    }

    #[test]
    fn gitlab_job_token_uses_job_token_header_not_bearer() {
        let client = Client::new();
        let req = GitLabAuth::JobToken("secret".into())
            .apply(client.get("https://gitlab.example.com/api/v4/projects/1/releases"))
            .build()
            .unwrap();
        assert_eq!(req.headers().get("JOB-TOKEN").unwrap(), "secret");
        assert!(req.headers().get("Authorization").is_none());
    }

    #[test]
    fn gitlab_private_token_uses_bearer_header() {
        let client = Client::new();
        let req = GitLabAuth::PrivateToken("secret".into())
            .apply(client.get("https://gitlab.example.com/api/v4/projects/1/releases"))
            .build()
            .unwrap();
        assert_eq!(req.headers().get("Authorization").unwrap(), "Bearer secret");
        assert!(req.headers().get("JOB-TOKEN").is_none());
    }

    #[test]
    fn extract_version_from_named_capture_when_present() {
        let flavor = Regex::new(r"geoip-db_(?P<version>[\.\d]+).tar").unwrap();
        let version = extract_version_from_flavor_capture(&flavor, "geoip-db_1.0.20260618.tar.xz");
        assert_eq!(version.as_deref(), Some("1.0.20260618"));
    }

    #[test]
    fn no_named_version_capture_keeps_none() {
        let flavor = Regex::new(r"geoip-db_[\.\d]+.tar").unwrap();
        let version = extract_version_from_flavor_capture(&flavor, "geoip-db_1.0.20260618.tar.xz");
        assert!(version.is_none());
    }

    #[test]
    fn extract_version_from_tag_normalizes_date_only_tag() {
        assert_eq!(extract_version_from_tag("2026-07-06"), "2026.07.06");
    }

    #[test]
    fn extract_version_from_tag_normalizes_date_suffix() {
        assert_eq!(extract_version_from_tag("release-2026-07-06"), "2026.07.06");
    }

    #[test]
    fn extract_version_from_tag_keeps_existing_formats() {
        assert_eq!(extract_version_from_tag("v1.2.3"), "1.2.3");
        assert_eq!(extract_version_from_tag("project-v1.2.3"), "1.2.3");
        assert_eq!(extract_version_from_tag("project-1.2.3"), "1.2.3");
    }
}
