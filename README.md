# debgen

A command-line tool for building Debian packages from upstream releases. It downloads assets from GitHub, GitLab, direct HTTP URLs or local directories, generates the full `debian/` packaging scaffolding, and runs `debuild` to produce `.deb` files.

## Features

- Download and extract release assets from GitHub, GitLab (including self-hosted instances), HTTP(S), or local directories (`file://`)
- Generate a pre-filled `debgen.yml` configuration from a project URL (fetches metadata from GitHub/GitLab APIs)
- Build Debian packages with automatic `debian/` directory generation (control, changelog, rules, copyright, etc.)
- Display package contents after a successful build (via `debc`, when verbosity is `-v` or higher)
- Inspect package metadata from any Debian repository
- Conditional builds: skip if upstream version is not newer than a given threshold or the version currently in a Debian repository (uses `dpkg --compare-versions`). If the package is not yet in the repo, the build proceeds. Supports `--inc` to force a rebuild with incremented debian revision when the upstream version matches.
- Optional version tagging with `--tag` / `DEBGEN_TAG` (produces versions like `1.2.3~1~myrepo`)
- Smart version extraction from release tags (handles `v1.2.3`, `project-v1.2.3`, `project-1.2.3`)
- Regex-based asset/link selection via `flavor`, including support for named capture `(?P<version>...)`
- HTTP(S) listing support (`text/html`, `text/plain`): resolves relative/absolute links, sorts matching basenames descending, and picks the latest match
- Authentication support for private GitHub/GitLab repositories
- Variable interpolation in configuration files (`{name}`, `{cwd}`, etc.)
- Colored, leveled logging output

## Requirements

- Rust 2024 edition (for building from source)
- System tools: `tar`, `xz`, `bzip2`, `unzip`, `gunzip` (for archive extraction)
- `debuild` and `debhelper` (for building packages)
- `dpkg` (for `--only-newer` version comparison)
- `dput` (optional, for uploading packages with `-U`)
- `debc` from `devscripts` (optional, for displaying package contents after build)

## Installation

```sh
cargo build --release
cp target/release/debgen /usr/local/bin/
```

## Usage

```
debgen [OPTIONS] <COMMAND>

Commands:
  build      Build a Debian package from a debgen.yml configuration file
  download   Download and extract a release
  init       Generate a debgen.yml configuration from a location URL
  checkrepo  Inspect package metadata from a Debian repository

Options:
  -v, --verbose...  Increase log verbosity (-v info, -vv debug, -vvv trace)
  -q, --quiet       Suppress all output except errors
  -h, --help        Print help
  -V, --version     Print version
```

## Commands

### init

Generate a `debgen.yml` configuration file pre-filled with metadata fetched from GitHub or GitLab APIs.

```sh
# From a GitHub project
debgen init "github://prometheus/node_exporter" --flavor linux-amd64

# From a GitLab project (gitlab.com)
debgen init "gitlab://inkscape/inkscape" --flavor linux-x86_64

# From a self-hosted GitLab instance
debgen init "gitlab://git.mycompany.com/mygroup/myproject" --flavor linux-amd64

# From a local directory
debgen init "file:///opt/myapp/src"

# Specify output directory
debgen init "github://jqlang/jq" --flavor linux-amd64 -o ./jq-pkg
```

The generated file includes the project description, homepage, license (GitHub), architecture (inferred from the flavor), and a commented-out `configure` block as a starting point.

### download

Download and extract a release asset into a target directory.

```sh
# GitHub release
debgen download "github://prometheus/node_exporter" --flavor linux-amd64 -o ./build

# GitLab release (gitlab.com)
debgen download "gitlab://somegroup/someproject" --flavor linux-amd64 -o ./build

# Self-hosted GitLab release
debgen download "gitlab://git.mycompany.com/mygroup/myproject" --flavor linux-amd64 -o ./build

# Direct HTTP URL
debgen download "https://example.com/app-1.0.tar.gz" -o ./build

# HTTP listing URL (index page)
debgen download "https://downloads.example.com/archives/" -F "myapp_(?P<version>[\\.\\d]+).tar" -o ./build

# Local directory (copies without altering source)
debgen download "file:///opt/myapp/src" -o ./build

# With authentication for private repos
debgen download "github://corp/private-tool" --flavor linux-amd64 \
  --github-token "$GITHUB_TOKEN" -o ./build
```

For `github://` and `gitlab://` locations, the `--flavor` argument is required.
For HTTP(S) listing URLs (`text/html` or `text/plain`), `--flavor` is also required so debgen can select the archive link.
For direct HTTP archive URLs, `--flavor` is optional, but a named capture like `(?P<version>...)` can still be used for version extraction.

### build

Build a Debian package from a `debgen.yml` configuration file. This downloads the upstream sources, generates the `debian/` directory, and runs `debuild`.

```sh
# Build using debgen.yml in the current directory
debgen build

# Specify a configuration file
debgen build mypackage.yml

# Only build if upstream version is newer than 1.9.0
debgen build -N 1.9.0

# Only build if upstream version is newer than what's in a Debian repo
debgen build -N "https://deb.debian.org/debian#trixie#main#amd64"

# Repo URL with defaults (dist=unstable, section=main, arch=amd64)
debgen build -N "https://my.repo.org/packages"

# Repo URL with only dist specified
debgen build -N "https://my.repo.org/packages#stable"

# Force rebuild with incremented debian revision if same upstream version in repo
debgen build -N "https://my.repo.org/packages#unstable" -I

# Append a version tag suffix
debgen build -T myrepo

# Specify a custom build output directory (default: build)
debgen build -O /tmp/mybuild

# Upload after build via scp
debgen build -U "scp://user@repo.example.com/var/spool/incoming"

# Upload with additional dput options as query params
debgen build -U "scp://user@repo.example.com/var/spool/incoming?hash=md5&allow_unsigned_uploads=1"

# Clean build artifacts after build and upload
debgen build -U "scp://user@repo.example.com/incoming" -C

# Keep the source tree after building
debgen build -S

# With authentication
debgen build --github-token "$GITHUB_TOKEN"

# Combine options
debgen build mypackage.yml -N 2.0.0 -O ./out -U "scp://deploy@repo.local/incoming" -C -S
```

The resulting `.deb` files are placed in the build output directory (default `build/`). When the log level is info or higher (`-v`), the package contents are displayed using `debc` after a successful build.

#### Upload URI format

The `-U/--upload` option accepts a URI that describes the dput target:

```
method://login@fqdn/incoming_path?key=value&key=value
```

| Component | Description |
|---|---|
| `method` | Transfer method: `scp`, `rsync`, `ftp`, `http`, `https`, `local` |
| `login` | (optional) Username for authentication |
| `fqdn` | Fully-qualified domain name of the target host |
| `/incoming_path` | Remote path for incoming packages |
| `?key=value&...` | (optional) Additional dput.cf options as query parameters |

Supported query parameters include: `hash` (md5, sha), `allow_unsigned_uploads` (0/1), `allowed_distributions`, `scp_compress` (0/1), and any other valid dput.cf option.

A temporary `dput.cf` is generated from the URI, passed to `dput -c`, and deleted after the upload.

### checkrepo

Query package metadata from a Debian repository index.

```sh
# Basic lookup (defaults: trixie, main, amd64)
debgen checkrepo https://deb.debian.org/debian vim

# Specify distribution, section, and architecture
debgen checkrepo https://deb.debian.org/debian nginx -d bookworm -s main -a arm64

# JSON output
debgen checkrepo https://deb.debian.org/debian curl -j

# Filter specific fields
debgen checkrepo https://deb.debian.org/debian vim -f Version,Package,Depends

# JSON output with filtered fields
debgen checkrepo https://deb.debian.org/debian vim -j -f Version,Package
```

## Configuration file

The `debgen.yml` file describes the package to build. All string values support `{variable}` interpolation.

### Required fields

| Field | Description |
|---|---|
| `name` | Package name |
| `description` | Short description |
| `homepage` | Upstream project URL |
| `contact` | Upstream contact (name and email) |
| `license` | SPDX license identifier |
| `location` | Source URL (`github://`, `gitlab://`, `http(s)://`, `file://`) |

### Optional fields

| Field | Default | Description |
|---|---|---|
| `flavor` | | Regex used to select release assets/links (GitHub, GitLab, and HTTP listings) |
| `maintainer` | auto-detected | Package maintainer (overrides `DEBFULLNAME`/`DEBEMAIL`) |
| `section` | `utils` | Debian section |
| `priority` | `optional` | Debian priority |
| `arch` | `all` | Target architecture |
| `depends` | `[]` | Runtime dependencies |
| `build-depends` | `[]` | Build dependencies |
| `dirs` | `[]` | Directories to create in the package |
| `files` | `{}` | Files to install (source: destination) |
| `configure` | | Pre-build configuration block (see below) |

### Configure block

The optional `configure` block allows file manipulation before building:

```yaml
configure:
  mkdir:
    - usr/share/myapp
  cp:
    source-file: destination-file
  mv:
    old-name: new-name
  content:
    etc/myapp/config.yml: |
      key: value
  postinst: |
    systemctl daemon-reload
  prerm: |
    systemctl stop myapp
```

Supported scripts: `preinst`, `postinst`, `prerm`, `postrm`.

### Full example

```yaml
name: node-exporter
description: Prometheus exporter for hardware and OS metrics
homepage: https://prometheus.io/
contact: Prometheus Team <prometheus-developers@googlegroups.com>
license: Apache-2.0
arch: amd64
location: github://prometheus/node_exporter
flavor: linux-amd64

section: net
priority: optional

depends:
  - adduser

build-depends: []

dirs:
  - usr/bin
  - etc/default

files:
  node_exporter: usr/bin/

configure:
  content:
    etc/default/node_exporter: |
      OPTIONS="--web.listen-address=:9100"
  postinst: |
    adduser --system --group --no-create-home node_exporter || true
```

## Environment variables

Several arguments can be configured through environment variables:

| Variable | Argument | Used by |
|---|---|---|
| `DEBGEN_CONFIG` | `<CONFIG>` | `build` |
| `DEBGEN_NEWER` | `-N, --only-newer` | `build` |
| `DEBGEN_UPLOAD` | `-U, --upload` | `build` |
| `DEBGEN_TAG` | `-T, --tag` | `build` |
| `DEBGEN_BUILD_DIR` | `-O, --output` | `build` |
| `DEBGEN_OUTPUT` | `-o, --output` | `download` |
| `DEBGEN_DIST` | `-d, --dist` | `checkrepo` |
| `DEBGEN_SECTION` | `-s, --section` | `checkrepo` |
| `DEBGEN_ARCH` | `-a, --arch` | `checkrepo` |
| `DEBFULLNAME` | (maintainer auto-detection fallback) | `build` |
| `DEBEMAIL` | (maintainer auto-detection fallback) | `build` |
| `EMAIL` | (maintainer auto-detection fallback) | `build` |
| `GITHUB_TOKEN` | `--github-token` | `build`, `download`, `init` |
| `GITLAB_TOKEN` | `--gitlab-token` | `build`, `download`, `init` |

## Location schemes

| Scheme | Format | Example |
|---|---|---|
| GitHub | `github://owner/repo` | `github://prometheus/node_exporter` |
| GitLab (gitlab.com) | `gitlab://group/repo` | `gitlab://inkscape/inkscape` |
| GitLab (self-hosted) | `gitlab://host/group/repo` | `gitlab://git.mycompany.com/mygroup/myproject` |
| HTTP(S) archive | `https://url/to/archive` | `https://example.com/app-1.0.tar.gz` |
| HTTP(S) listing | `https://url/to/index/` | `https://geo.kaizen-hosting.com/archives/` |
| Local | `file:///absolute/path` | `file:///opt/myapp/src` |

For `gitlab://` URLs, if the first path segment contains a dot (e.g. `git.mycompany.com`), it is treated as a custom hostname. Otherwise, `gitlab.com` is used as the default host.

For GitHub and GitLab locations, the `flavor` field (or `--flavor` argument) is required to select the correct release asset.
For HTTP(S) listing locations (`text/html`, `text/plain`), `flavor` is required to select the archive link from the page.
The flavor value is treated as a regular expression; if the regex is invalid, it is treated as a literal string.
Matching is done against asset names/basenames. For HTTP listings, links can be absolute or relative.
When multiple links match, debgen sorts matching basenames in descending lexicographic order and picks the first.

## Version detection

Version detection order:

1. If `flavor` contains a named capture `(?P<version>...)` and the selected asset/link matches it, that capture is used.
2. Otherwise, for GitHub/GitLab locations, the upstream version is extracted from the release tag name.
3. Otherwise, for direct HTTP archive URLs, if `flavor` contains a named capture `(?P<version>...)` and the archive basename matches it, that capture is used.
4. Otherwise (including `file://` downloads), no upstream version is inferred.

Release-tag extraction supports the following formats:

| Tag format | Detected version |
|---|---|
| `v1.2.3` | `1.2.3` |
| `1.2.3` | `1.2.3` |
| `myproject-v1.2.3` | `1.2.3` |
| `myproject-1.2.3` | `1.2.3` |
| `2026-07-06` | `2026.07.06` |
| `release-2026-07-06` | `2026.07.06` |

This is particularly useful for monorepos where tags are prefixed with the project name.

## Supported archive formats

Extraction is performed using system tools:

| Format | Tool required |
|---|---|
| `.tar.gz`, `.tgz` | `tar`, `gunzip` |
| `.tar.xz`, `.txz` | `tar`, `xz` |
| `.tar.bz2`, `.tbz2` | `tar`, `bzip2` |
| `.tar` | `tar` |
| `.zip` | `unzip` |

## License

MIT
