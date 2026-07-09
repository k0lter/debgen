use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use tracing::debug;
use tracing::info;

#[derive(Debug, Deserialize, Clone)]
pub struct ConfigureBlock {
    #[serde(default)]
    pub mkdir: Option<Vec<String>>,
    #[serde(default)]
    pub content: Option<HashMap<String, String>>,
    #[serde(default)]
    pub cp: Option<HashMap<String, String>>,
    #[serde(default)]
    pub mv: Option<HashMap<String, String>>,
    #[serde(default)]
    pub preinst: Option<String>,
    #[serde(default)]
    pub prerm: Option<String>,
    #[serde(default)]
    pub postinst: Option<String>,
    #[serde(default)]
    pub postrm: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DebgenConfig {
    pub name: String,
    pub description: String,
    pub homepage: String,
    pub contact: String,
    pub license: String,
    pub location: String,

    #[serde(default)]
    pub flavor: Option<String>,
    #[serde(default)]
    pub maintainer: Option<String>,
    #[serde(default = "default_section")]
    pub section: String,
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default = "default_arch")]
    pub arch: String,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(rename = "build-depends", default)]
    pub build_depends: Vec<String>,
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default)]
    pub files: HashMap<String, String>,
    #[serde(default)]
    pub configure: Option<ConfigureBlock>,
}

fn default_section() -> String {
    "utils".to_string()
}
fn default_priority() -> String {
    "optional".to_string()
}
fn default_arch() -> String {
    "all".to_string()
}

/// Recursively interpolate `{key}` placeholders in strings using `vars`.
pub fn interpolate_string(s: &str, vars: &HashMap<String, String>) -> String {
    let mut result = s.to_string();
    for (key, value) in vars {
        let pattern = format!("{{{}}}", key);
        result = result.replace(&pattern, value);
    }
    result
}

pub fn interpolate_vec(v: &[String], vars: &HashMap<String, String>) -> Vec<String> {
    v.iter().map(|s| interpolate_string(s, vars)).collect()
}

pub fn interpolate_map(
    m: &HashMap<String, String>,
    vars: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for (key, value) in m {
        let interp_value = interpolate_string(value, vars);
        let interp_key = interpolate_string(key, vars);
        result.insert(interp_key, interp_value);
    }
    result
}

fn require_non_empty(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        crate::error_msg!(
            "Required field [field]{}[/] is missing or empty in configuration",
            field
        );
    }
    Ok(())
}

impl DebgenConfig {
    pub fn load(path: &Path) -> Result<Self> {
        info!(
            "[action]Reading and parsing[/] configuration file [path]{}[/]",
            path.display()
        );

        let content = fs::read_to_string(path).context(format!(
            "Unable to load yaml config file [path]{}[/]",
            path.display()
        ))?;

        let mut cfg: DebgenConfig = serde_yaml::from_str(&content)
            .context("Failed to parse [field]YAML[/] configuration")?;

        require_non_empty(&cfg.name, "name")?;
        require_non_empty(&cfg.description, "description")?;
        require_non_empty(&cfg.homepage, "homepage")?;
        require_non_empty(&cfg.contact, "contact")?;
        require_non_empty(&cfg.license, "license")?;
        require_non_empty(&cfg.location, "location")?;

        let cwd = std::env::current_dir()
            .context("Failed to determine current working [path]directory[/]")?
            .to_string_lossy()
            .to_string();

        let mut vars: HashMap<String, String> = HashMap::new();
        vars.insert("cwd".to_string(), cwd);

        cfg.name = interpolate_string(&cfg.name, &vars);
        vars.insert("name".to_string(), cfg.name.clone());

        cfg.description = interpolate_string(&cfg.description, &vars);
        vars.insert("description".to_string(), cfg.description.clone());

        cfg.homepage = interpolate_string(&cfg.homepage, &vars);
        vars.insert("homepage".to_string(), cfg.homepage.clone());

        cfg.contact = interpolate_string(&cfg.contact, &vars);
        vars.insert("contact".to_string(), cfg.contact.clone());

        cfg.license = interpolate_string(&cfg.license, &vars);
        vars.insert("license".to_string(), cfg.license.clone());

        cfg.location = interpolate_string(&cfg.location, &vars);
        vars.insert("location".to_string(), cfg.location.clone());

        cfg.section = interpolate_string(&cfg.section, &vars);
        vars.insert("section".to_string(), cfg.section.clone());

        cfg.priority = interpolate_string(&cfg.priority, &vars);
        vars.insert("priority".to_string(), cfg.priority.clone());

        cfg.arch = interpolate_string(&cfg.arch, &vars);
        vars.insert("arch".to_string(), cfg.arch.clone());

        cfg.depends = interpolate_vec(&cfg.depends, &vars);
        cfg.build_depends = interpolate_vec(&cfg.build_depends, &vars);
        cfg.dirs = interpolate_vec(&cfg.dirs, &vars);
        cfg.files = interpolate_map(&cfg.files, &vars);

        if let Some(ref flav) = cfg.flavor {
            cfg.flavor = Some(interpolate_string(flav, &vars));
        }

        if let Some(ref maint) = cfg.maintainer {
            cfg.maintainer = Some(interpolate_string(maint, &vars));
        }

        if let Some(ref mut configure) = cfg.configure {
            if let Some(ref dirs) = configure.mkdir {
                configure.mkdir = Some(interpolate_vec(dirs, &vars));
            }
            if let Some(ref content) = configure.content {
                configure.content = Some(interpolate_map(content, &vars));
            }
            if let Some(ref cp) = configure.cp {
                configure.cp = Some(interpolate_map(cp, &vars));
            }
            if let Some(ref mv) = configure.mv {
                configure.mv = Some(interpolate_map(mv, &vars));
            }
            if let Some(ref s) = configure.preinst {
                configure.preinst = Some(interpolate_string(s, &vars));
            }
            if let Some(ref s) = configure.prerm {
                configure.prerm = Some(interpolate_string(s, &vars));
            }
            if let Some(ref s) = configure.postinst {
                configure.postinst = Some(interpolate_string(s, &vars));
            }
            if let Some(ref s) = configure.postrm {
                configure.postrm = Some(interpolate_string(s, &vars));
            }
        }

        debug!("Configuration loaded for package: [pkg]{}[/]", cfg.name);
        Ok(cfg)
    }

    /// Re-interpolate fields that may reference `{version}` after the upstream
    /// version has been detected (post-download).
    pub fn interpolate_version(&mut self, version: &str) {
        let mut vars = HashMap::new();
        vars.insert("version".to_string(), version.to_string());

        self.dirs = interpolate_vec(&self.dirs, &vars);
        self.files = interpolate_map(&self.files, &vars);

        if let Some(ref mut configure) = self.configure {
            if let Some(ref dirs) = configure.mkdir {
                configure.mkdir = Some(interpolate_vec(dirs, &vars));
            }
            if let Some(ref content) = configure.content {
                configure.content = Some(interpolate_map(content, &vars));
            }
            if let Some(ref cp) = configure.cp {
                configure.cp = Some(interpolate_map(cp, &vars));
            }
            if let Some(ref mv) = configure.mv {
                configure.mv = Some(interpolate_map(mv, &vars));
            }
            if let Some(ref s) = configure.preinst {
                configure.preinst = Some(interpolate_string(s, &vars));
            }
            if let Some(ref s) = configure.prerm {
                configure.prerm = Some(interpolate_string(s, &vars));
            }
            if let Some(ref s) = configure.postinst {
                configure.postinst = Some(interpolate_string(s, &vars));
            }
            if let Some(ref s) = configure.postrm {
                configure.postrm = Some(interpolate_string(s, &vars));
            }
        }

        debug!(
            "Interpolated [field]{{version}}[/] = [version]{}[/] in config fields",
            version
        );
    }
}
