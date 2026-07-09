#[macro_use]
mod logger;

mod builder;
mod checkrepo;
mod config;
mod download;
mod init;
mod version;

use std::path::PathBuf;
use std::process;

use clap::builder::styling::{AnsiColor, Styles};
use clap::{Parser, Subcommand};

use tracing::{debug, error, info};

use crate::builder::DebPkgBuilder;
use crate::config::DebgenConfig;
use crate::download::{AuthTokens, parse_download_url, perform_download};

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Cyan.on_default().bold())
    .placeholder(AnsiColor::Yellow.on_default());

#[derive(Parser)]
#[command(
    name = "debgen",
    version,
    about = "Debian package builder, release downloader and repository inspector",
    styles = STYLES,
)]
struct Cli {
    /// Increase log verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Suppress all output except errors
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build a Debian package from a debgen.yml configuration file
    Build {
        /// Path to the YAML configuration file
        #[arg(default_value = "debgen.yml", env = "DEBGEN_CONFIG")]
        config: PathBuf,

        /// Only build if upstream version is newer than a threshold.
        /// Can be a version string (e.g. "1.9.0") or a Debian repo URL
        /// (e.g. "http://repo#dist#section#arch") to fetch the current version.
        /// Fragments default: dist=unstable, section=main, arch=amd64.
        #[arg(short = 'N', long = "only-newer")]
        only_newer: Option<String>,

        /// Force rebuild when upstream version matches repo version,
        /// incrementing the packaging revision (e.g. 1.2.3~1 -> 1.2.3~2).
        /// Requires -N with a repo URL.
        #[arg(short = 'I', long = "inc")]
        increment: bool,

        /// Append a tilde tag suffix to the package version (e.g. myrepo -> 1.2.3~1~myrepo).
        #[arg(short = 'T', long = "tag", env = "DEBGEN_TAG")]
        tag: Option<String>,

        /// Build output directory
        #[arg(short = 'O', long, default_value = "build", env = "DEBGEN_BUILD_DIR")]
        output: PathBuf,

        /// Upload resulting packages via dput.
        /// URI format: method://login@fqdn/incoming?key=value&...
        /// Supported query params: hash, allow_unsigned_uploads, allowed_distributions
        #[arg(short = 'U', long)]
        upload: Option<String>,

        /// Clean build artifacts after build (and upload if -U is set)
        #[arg(short = 'C', long)]
        clean: bool,

        /// Keep downloaded sources after build
        #[arg(short = 'S', long = "keep-sources")]
        keep_sources: bool,

        /// GitHub personal access token for private repositories
        #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
        github_token: Option<String>,

        /// GitLab personal access token for private repositories
        #[arg(long, env = "GITLAB_TOKEN", hide_env_values = true)]
        gitlab_token: Option<String>,
    },
    /// Download and extract a release from GitHub, GitLab, a local path, or a direct URL
    Download {
        /// Location URL (github://owner/repo, gitlab://[host/]group/repo, file:///path, or https://...)
        url: String,

        /// Asset name pattern to match in GitHub/GitLab releases
        #[arg(short = 'F', long)]
        flavor: Option<String>,

        /// Target directory for extraction
        #[arg(short, long, default_value = ".", env = "DEBGEN_OUTPUT")]
        output: PathBuf,

        /// GitHub personal access token for private repositories
        #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
        github_token: Option<String>,

        /// GitLab personal access token for private repositories
        #[arg(long, env = "GITLAB_TOKEN", hide_env_values = true)]
        gitlab_token: Option<String>,
    },
    /// Inspect package metadata from a Debian repository
    Checkrepo {
        /// Debian repository base URL
        repo: String,

        /// Package name to look up
        package: String,

        /// Distribution name
        #[arg(short, long, default_value = "trixie", env = "DEBGEN_DIST")]
        dist: String,

        /// Repository section
        #[arg(short, long, default_value = "main", env = "DEBGEN_SECTION")]
        section: String,

        /// Architecture
        #[arg(short, long, default_value = "amd64", env = "DEBGEN_ARCH")]
        arch: String,

        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,

        /// Filter displayed fields (comma-separated)
        #[arg(short = 'f', long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
    },
    /// Generate a debgen.yml configuration from a location URL
    Init {
        /// Location URL (github://owner/repo, gitlab://[host/]group/repo, file:///path, or https://...)
        location: String,

        /// Asset name pattern to match in GitHub/GitLab releases
        #[arg(short = 'F', long)]
        flavor: Option<String>,

        /// Output directory for the generated debgen.yml
        #[arg(short, long, default_value = ".")]
        output: PathBuf,

        /// GitHub personal access token for private repositories
        #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
        github_token: Option<String>,

        /// GitLab personal access token for private repositories
        #[arg(long, env = "GITLAB_TOKEN", hide_env_values = true)]
        gitlab_token: Option<String>,
    },
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    logger::init(cli.verbose, cli.quiet);

    match cli.command {
        Commands::Build {
            config,
            only_newer,
            increment,
            tag,
            output,
            upload,
            clean,
            keep_sources,
            github_token,
            gitlab_token,
        } => {
            info!(
                "[action]Starting[/] Debian package build from [path]{}[/]",
                config.display()
            );
            let cfg = DebgenConfig::load(&config)?;
            let tokens = AuthTokens {
                github: github_token,
                gitlab: gitlab_token,
            };
            let mut builder = DebPkgBuilder::new(
                cfg,
                only_newer,
                increment,
                tag,
                keep_sources,
                tokens,
                output.clone(),
            );
            builder.build()?;
            info!("[ok]Build completed successfully[/]");

            if let Some(ref uri) = upload {
                builder.upload(uri)?;
            }

            if clean {
                builder.clean()?;
            }
        }
        Commands::Download {
            url,
            flavor,
            output,
            github_token,
            gitlab_token,
        } => {
            let parsed = parse_download_url(&url)?;
            debug!("Download target: [field]{:?}[/]", parsed);
            std::fs::create_dir_all(&output)?;
            let tokens = AuthTokens {
                github: github_token,
                gitlab: gitlab_token,
            };
            let result = perform_download(&parsed, &output, flavor.as_deref(), &tokens)?;
            info!("Downloaded to [path]{}[/]", result.extract_path.display());
            if let Some(ref version) = result.version {
                info!("Version: [version]{}[/]", version);
            }
        }
        Commands::Checkrepo {
            repo,
            package,
            dist,
            section,
            arch,
            json,
            fields,
        } => {
            checkrepo::run(&repo, &package, &dist, &section, &arch, json, &fields)?;
        }
        Commands::Init {
            location,
            flavor,
            output,
            github_token,
            gitlab_token,
        } => {
            let tokens = AuthTokens {
                github: github_token,
                gitlab: gitlab_token,
            };
            init::run(&location, flavor.as_deref(), &output, &tokens)?;
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        error!("{:#}", e);
        process::exit(1);
    }
}
