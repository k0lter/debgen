use std::path::Path;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::download::{AuthTokens, GitLabAuth, ParsedUrl, parse_download_url};
use tracing::{debug, info, warn};

const CONFIG_FILENAME: &str = "debgen.yml";

#[derive(Default)]
struct RepoMeta {
    name: String,
    description: String,
    homepage: String,
    license: String,
    contact: String,
    arch: String,
}

// ---------- GitHub API ----------

#[derive(Deserialize)]
struct GhRepo {
    description: Option<String>,
    homepage: Option<String>,
    html_url: Option<String>,
    license: Option<GhLicense>,
}

#[derive(Deserialize)]
struct GhLicense {
    spdx_id: Option<String>,
}

fn fetch_github_meta(project: &str, token: Option<&str>) -> Result<RepoMeta> {
    let client = build_client()?;
    let url = format!("https://api.github.com/repos/{}", project);
    debug!(
        "[action]Fetching[/] GitHub repo metadata from [url]{}[/]",
        url
    );

    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {}", t));
    }

    let gh: GhRepo = req
        .send()
        .context(format!(
            "Failed to reach GitHub API for [pkg]{}[/]",
            project
        ))?
        .error_for_status()
        .context(format!("GitHub API error for [pkg]{}[/]", project))?
        .json()
        .context("Failed to parse GitHub API [field]response[/]")?;

    let name = project
        .rsplit('/')
        .next()
        .unwrap_or("mypackage")
        .to_lowercase();

    let homepage = gh
        .homepage
        .filter(|h| !h.is_empty())
        .or(gh.html_url)
        .unwrap_or_else(|| format!("https://github.com/{}", project));

    let license = gh
        .license
        .and_then(|l| l.spdx_id)
        .filter(|s| s != "NOASSERTION")
        .unwrap_or_default();

    let description = gh.description.unwrap_or_default();

    Ok(RepoMeta {
        name,
        description,
        homepage,
        license,
        ..Default::default()
    })
}

// ---------- GitLab API ----------

#[derive(Deserialize)]
struct GlProject {
    description: Option<String>,
    web_url: Option<String>,
    name: Option<String>,
}

fn fetch_gitlab_meta(host: &str, project: &str, token: Option<&GitLabAuth>) -> Result<RepoMeta> {
    let client = build_client()?;
    let encoded = project.replace('/', "%2F");
    let url = format!("https://{}/api/v4/projects/{}", host, encoded);
    debug!(
        "[action]Fetching[/] GitLab project metadata from [url]{}[/]",
        url
    );

    let mut req = client.get(&url);
    if let Some(auth) = token {
        req = auth.apply(req);
    }

    let gl: GlProject = req
        .send()
        .context(format!(
            "Failed to reach GitLab API at [url]{}[/] for [pkg]{}[/]",
            host, project
        ))?
        .error_for_status()
        .context(format!(
            "GitLab API error for [pkg]{}[/] on [url]{}[/]",
            project, host
        ))?
        .json()
        .context("Failed to parse GitLab API [field]response[/]")?;

    let name = gl
        .name
        .unwrap_or_else(|| {
            project
                .rsplit('/')
                .next()
                .unwrap_or("mypackage")
                .to_string()
        })
        .to_lowercase();

    let homepage = gl
        .web_url
        .unwrap_or_else(|| format!("https://{}/{}", host, project));

    let description = gl.description.unwrap_or_default();

    Ok(RepoMeta {
        name,
        description,
        homepage,
        ..Default::default()
    })
}

// ---------- Helpers ----------

fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent("Debgen-Init/1.0")
        .build()
        .context("Failed to build HTTP client")
}

fn derive_arch_from_str(s: &str) -> &'static str {
    let lower = s.to_lowercase();
    if lower.contains("amd64") || lower.contains("x86_64") || lower.contains("x64") {
        "amd64"
    } else if lower.contains("arm64") || lower.contains("aarch64") {
        "arm64"
    } else if lower.contains("armhf") || lower.contains("armv7") {
        "armhf"
    } else if lower.contains("i386") || lower.contains("i686") || lower.contains("x86") {
        "i386"
    } else {
        "all"
    }
}

fn derive_arch(parsed: &ParsedUrl, flavor: Option<&str>) -> &'static str {
    if let Some(f) = flavor {
        return derive_arch_from_str(f);
    }
    match parsed {
        ParsedUrl::Http(url) => derive_arch_from_str(url),
        ParsedUrl::File(path) => derive_arch_from_str(&path.to_string_lossy()),
        _ => "all",
    }
}

fn fallback_meta(parsed: &ParsedUrl) -> RepoMeta {
    let name = match parsed {
        ParsedUrl::GitHub(project) | ParsedUrl::GitLab { project, .. } => project
            .rsplit('/')
            .next()
            .unwrap_or("mypackage")
            .to_lowercase(),
        ParsedUrl::Http(url) => url
            .rsplit('/')
            .next()
            .unwrap_or("mypackage")
            .split('.')
            .next()
            .unwrap_or("mypackage")
            .to_lowercase(),
        ParsedUrl::File(path) => path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "mypackage".to_string()),
    };

    let homepage = match parsed {
        ParsedUrl::GitHub(project) => format!("https://github.com/{}", project),
        ParsedUrl::GitLab { host, project } => format!("https://{}/{}", host, project),
        ParsedUrl::Http(url) => url.clone(),
        ParsedUrl::File(path) => path.to_string_lossy().to_string(),
    };

    RepoMeta {
        name,
        homepage,
        ..Default::default()
    }
}

fn or_todo(value: &str, hint: &str) -> String {
    if value.is_empty() {
        format!("TODO - {}", hint)
    } else {
        value.to_string()
    }
}

/// Generate a debgen.yml configuration file pre-filled from a location URL.
pub fn run(location: &str, flavor: Option<&str>, output: &Path, tokens: &AuthTokens) -> Result<()> {
    let parsed = parse_download_url(location)?;

    let dest = output.join(CONFIG_FILENAME);
    if dest.exists() {
        crate::error_msg!("[path]{}[/] already exists, aborting", dest.display());
    }

    let mut meta = match &parsed {
        ParsedUrl::GitHub(project) => {
            info!(
                "[action]Fetching[/] metadata from GitHub for [pkg]{}[/]",
                project
            );
            fetch_github_meta(project, tokens.github.as_deref()).unwrap_or_else(|e| {
                warn!("Could not fetch GitHub metadata: {}", e);
                fallback_meta(&parsed)
            })
        }
        ParsedUrl::GitLab { host, project } => {
            info!(
                "[action]Fetching[/] metadata from GitLab ([url]{}[/]) for [pkg]{}[/]",
                host, project
            );
            fetch_gitlab_meta(host, project, tokens.gitlab.as_ref()).unwrap_or_else(|e| {
                warn!("Could not fetch GitLab metadata: {}", e);
                fallback_meta(&parsed)
            })
        }
        ParsedUrl::Http(_) | ParsedUrl::File(_) => fallback_meta(&parsed),
    };

    meta.arch = derive_arch(&parsed, flavor).to_string();
    meta.description = or_todo(&meta.description, "Package description");
    meta.license = or_todo(&meta.license, "License (e.g. MIT, Apache-2.0, GPL-3)");
    meta.contact = or_todo(&meta.contact, "Your Name <your@email.com>");

    let flavor_line = match flavor {
        Some(f) => format!("flavor: {}", f),
        None => "# flavor: pattern-to-match-release-asset".to_string(),
    };

    let yaml = format!(
        r#"name: {name}
description: {description}
homepage: {homepage}
contact: {contact}
license: {license}
arch: {arch}
location: {location}
{flavor_line}

# section: utils
# priority: optional

depends: []
build-depends: []

dirs:
  - usr/bin

files:
  {name}: usr/bin/

# configure:
#   mkdir:
#     - usr/bin
#   cp:
#     src: dst
#   mv:
#     src: dst
#   content:
#     path/to/file: "file content"
#   postinst: |
#     echo "Post-installation script"
"#,
        name = meta.name,
        description = meta.description,
        homepage = meta.homepage,
        contact = meta.contact,
        license = meta.license,
        arch = meta.arch,
        location = location,
        flavor_line = flavor_line,
    );

    std::fs::create_dir_all(output).context(format!(
        "Failed to create output directory [path]{}[/]",
        output.display()
    ))?;
    std::fs::write(&dest, &yaml).context(format!("Failed to write [path]{}[/]", dest.display()))?;
    info!("[ok]Generated[/] [path]{}[/]", dest.display());

    Ok(())
}
