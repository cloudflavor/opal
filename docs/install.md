# Install

## Install with Cargo

```bash
cargo install --path crates/opal
```

From this workspace, install the CLI crate directly.

The installed executable is still:

```bash
opal
```

## Homebrew on macOS

If you prefer Homebrew, install Opal from the Cloudflavor tap:

```bash
brew tap cloudflavor/tap
brew install cloudflavor/tap/opal-cli
```

Verify the install:

```bash
opal --version
```

### Upgrade

When the tap publishes a newer formula:

```bash
brew update
brew upgrade cloudflavor/tap/opal-cli
```

### Check versions

Use Homebrew's built-in inspection commands:

```bash
brew livecheck cloudflavor/tap/opal-cli
brew info cloudflavor/tap/opal-cli
```

### Remove

To uninstall Opal and remove the tap:

```bash
brew uninstall cloudflavor/tap/opal-cli
brew untap cloudflavor/tap
```

## Install from a local checkout

```bash
cargo install --path crates/opal
```

Use this when you are developing Opal itself from a local repository checkout.

## Download prebuilt binaries

Current release artifact targets:

- `aarch64-apple-silicon`
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-gnu`

Release downloads are published under:

```text
{{github_releases_url}}
```

Current release examples for `{{release_tag}}`:

```bash
# macOS Apple Silicon
curl -L {{release_asset_url_macos_arm64}} | tar xz

# Linux ARM64
curl -L {{release_asset_url_linux_arm64}} | tar xz

# Linux AMD64
curl -L {{release_asset_url_linux_amd64}} | tar xz
```

## Runtime requirements

Opal does not ship its own container runtime. It wraps the local engine CLIs listed below, so the engine you want to use must already be installed and available on your `PATH`.

Supported local engines:

- macOS:
  - Apple `container` CLI — https://github.com/apple/container
  - Docker CLI
  - Podman CLI
  - OrbStack/Docker-compatible CLI
- Linux:
  - Docker CLI
  - Podman CLI
  - Nerdctl CLI

`nerdctl` is Linux-oriented rather than a first-class macOS host engine.

Default engine selection:

- macOS: `auto` uses Apple `container`
- Linux: `auto` uses `podman`

You can override that `auto` default in config with:

```toml
[engine]
default = "docker"
```

In practice, that means:

- `--engine container` expects Apple `container`
- `--engine docker` expects Docker
- `--engine podman` expects Podman
- `--engine orbstack` expects an OrbStack-backed Docker-compatible CLI
- `--engine nerdctl` expects Nerdctl

If you want the Apple engine specifically, install it from:

```text
https://github.com/apple/container
```

If the selected engine CLI is missing or unavailable, Opal cannot run jobs with that engine.

## Why prefer Apple `container` on macOS?

On macOS, many Linux-container workflows run all containers inside one shared Linux VM.

Apple `container` uses a different model:

- each container runs in its own lightweight VM
- only the host data you explicitly mount is exposed to that container VM
- you get stronger isolation properties than a shared-VM container setup
- Apple documents performance as being comparable to shared-VM container workflows while using lightweight per-container VMs instead of full traditional VMs

For Opal's local-debugging use case, that gives a few practical benefits:

- better isolation between jobs and ad-hoc troubleshooting containers
- less accidental host-data exposure, because mounts stay explicit per job
- resource tuning that maps naturally to each job when you use Opal's `[container]` config

This does not make Apple `container` universally better for every workflow, but it is a strong default for local macOS pipeline runs when you want tight per-job isolation without running one large always-on Linux VM for all containers.

## Verify the install

```bash
opal --version
opal plan --no-pager
```
