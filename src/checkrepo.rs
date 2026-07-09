use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use tracing::{debug, info};

use crate::download::pipe_decompress;
use crate::logger::render_markup;

type PackageMeta = BTreeMap<String, String>;

fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent("Debgen-Checkrepo/1.0")
        .build()
        .context("Failed to build HTTP client")
}

/// Fetch and decompress the Packages index from a Debian repository.
/// Tries Packages.xz first, then Packages.gz, then uncompressed Packages.
fn fetch_packages_index(repo_url: &str, dist: &str, section: &str, arch: &str) -> Result<String> {
    let base = format!(
        "{}/dists/{}/{}/binary-{}",
        repo_url.trim_end_matches('/'),
        dist,
        section,
        arch
    );

    let client = build_client()?;

    let formats = [
        ("Packages.xz", "xz"),
        ("Packages.gz", "gz"),
        ("Packages", "plain"),
    ];

    for (filename, compression) in &formats {
        let url = format!("{}/{}", base, filename);
        debug!("[action]Trying[/] [url]{}[/]", url);

        let response = client.get(&url).send();
        let response = match response {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };

        info!("Fetched index from [url]{}[/]", url);

        return match *compression {
            "xz" => {
                let mut compressed = Vec::new();
                response
                    .take(100 * 1024 * 1024)
                    .read_to_end(&mut compressed)
                    .context("Failed to read [path]Packages.xz[/] response body")?;
                let decompressed = pipe_decompress("xz", &["-d", "--stdout"], &compressed)?;
                String::from_utf8(decompressed)
                    .context("Invalid UTF-8 in decompressed [path]Packages[/]")
            }
            "gz" => {
                let mut compressed = Vec::new();
                response
                    .take(100 * 1024 * 1024)
                    .read_to_end(&mut compressed)
                    .context("Failed to read [path]Packages.gz[/] response body")?;
                let decompressed = pipe_decompress("gunzip", &["-c"], &compressed)?;
                String::from_utf8(decompressed)
                    .context("Invalid UTF-8 in decompressed [path]Packages[/]")
            }
            _ => response
                .text()
                .context("Failed to read [path]Packages[/] file"),
        };
    }

    crate::error_msg!(
        "Failed to fetch Packages index from [url]{}/Packages[.xz|.gz][/]",
        base
    );
}

/// Parse a Debian Packages index into stanzas (one per package).
/// Each stanza is a BTreeMap of field_name -> value (with continuation lines joined).
fn parse_packages_index(content: &str) -> Vec<PackageMeta> {
    let mut packages = Vec::new();
    let mut current: PackageMeta = BTreeMap::new();
    let mut last_key: Option<String> = None;

    for line in content.lines() {
        if line.is_empty() {
            if !current.is_empty() {
                packages.push(current);
                current = BTreeMap::new();
                last_key = None;
            }
            continue;
        }

        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(ref key) = last_key
                && let Some(val) = current.get_mut(key)
            {
                val.push('\n');
                val.push_str(line);
            }
            continue;
        }

        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].to_string();
            let value = line[colon_pos + 1..].trim_start().to_string();
            last_key = Some(key.clone());
            current.insert(key, value);
        }
    }

    if !current.is_empty() {
        packages.push(current);
    }

    packages
}

/// Find a package by exact name in the parsed index.
fn find_package<'a>(packages: &'a [PackageMeta], name: &str) -> Option<&'a PackageMeta> {
    packages
        .iter()
        .find(|p| p.get("Package").map(|n| n == name).unwrap_or(false))
}

/// Print package metadata with styled key/value output.
fn print_styled(meta: &PackageMeta, fields: &Option<Vec<String>>) {
    let entries: Vec<(&String, &String)> = match fields {
        Some(filter) => meta
            .iter()
            .filter(|(k, _)| filter.iter().any(|f| f.eq_ignore_ascii_case(k)))
            .collect(),
        None => meta.iter().collect(),
    };

    for (key, value) in entries {
        if value.contains('\n') {
            println!("{}:", render_markup(&format!("[field]{}[/]", key)));
            for line in value.lines() {
                if line.starts_with(' ') || line.starts_with('\t') {
                    println!("{}", render_markup(&format!("[dim]{}[/]", line)));
                } else {
                    println!("{}", line);
                }
            }
        } else {
            println!(
                "{}: {}",
                render_markup(&format!("[field]{}[/]", key)),
                render_markup(&format!("[value]{}[/]", value))
            );
        }
    }
}

/// Print package metadata as JSON.
fn print_json(meta: &PackageMeta, fields: &Option<Vec<String>>) -> Result<()> {
    let filtered: BTreeMap<&String, &String> = match fields {
        Some(filter) => meta
            .iter()
            .filter(|(k, _)| filter.iter().any(|f| f.eq_ignore_ascii_case(k)))
            .collect(),
        None => meta.iter().collect(),
    };

    let json = serde_json::to_string_pretty(&filtered)
        .context("Failed to serialize metadata to [field]JSON[/]")?;
    println!("{}", json);
    Ok(())
}

/// Look up the current version of a package in a Debian repository.
/// Returns None if the package is not found in the index.
pub fn get_version(
    repo_url: &str,
    package: &str,
    dist: &str,
    section: &str,
    arch: &str,
) -> Result<Option<String>> {
    let content = fetch_packages_index(repo_url, dist, section, arch)?;
    let packages = parse_packages_index(&content);
    let meta = match find_package(&packages, package) {
        Some(m) => m,
        None => return Ok(None),
    };
    Ok(meta.get("Version").cloned())
}

/// Entry point for the checkrepo command.
pub fn run(
    repo_url: &str,
    package: &str,
    dist: &str,
    section: &str,
    arch: &str,
    json: bool,
    fields: &Option<Vec<String>>,
) -> Result<()> {
    info!(
        "[action]Checking[/] package [pkg]{}[/] in [url]{}/dists/{}/{}/binary-{}[/]",
        package, repo_url, dist, section, arch
    );

    let content = fetch_packages_index(repo_url, dist, section, arch)?;
    debug!("Parsed Packages index ([value]{}[/] bytes)", content.len());

    let packages = parse_packages_index(&content);
    debug!("Found [value]{}[/] packages in index", packages.len());

    let meta = find_package(&packages, package);
    let meta = match meta {
        Some(m) => m,
        None => crate::error_msg!(
            "Package [pkg]{}[/] not found in [url]{}/dists/{}/{}/binary-{}[/]",
            package,
            repo_url,
            dist,
            section,
            arch
        ),
    };

    info!(
        "Found package [pkg]{}[/] version [version]{}[/]",
        package,
        meta.get("Version").unwrap_or(&"unknown".to_string())
    );

    if json {
        print_json(meta, fields)?;
    } else {
        print_styled(meta, fields);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_packages_index_supports_multiline_fields() {
        let content = "\
Package: demo
Version: 1.2.3
Description: first line
 second line

Package: other
Version: 2.0.0
";

        let packages = parse_packages_index(content);
        assert_eq!(packages.len(), 2);
        assert_eq!(
            packages[0].get("Description").map(String::as_str),
            Some("first line\n second line")
        );
    }

    #[test]
    fn find_package_matches_exact_name() {
        let content = "\
Package: demo
Version: 1.2.3

Package: demo-tools
Version: 2.0.0
";
        let packages = parse_packages_index(content);
        let meta = find_package(&packages, "demo").expect("package exists");
        assert_eq!(meta.get("Version").map(String::as_str), Some("1.2.3"));
        assert!(find_package(&packages, "missing").is_none());
    }
}
