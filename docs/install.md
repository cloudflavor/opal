# Install

## Install from crates.io

```bash
cargo install opal-cli
```

This is the default user-facing install path.

The installed executable is still:

```bash
opal
```

## Install with Homebrew on macOS

```bash
brew tap cloudflavor/tap
brew install cloudflavor/tap/opal
```

### `#brew-macos`

Use it:

```bash
brew tap cloudflavor/tap
brew install cloudflavor/tap/opal
opal --version
```

Update it:

```bash
brew update
brew upgrade cloudflavor/tap/opal
```

Check versions:

```bash
brew livecheck cloudflavor/tap/opal
brew info cloudflavor/tap/opal
```

Remove it:

```bash
brew uninstall cloudflavor/tap/opal
brew untap cloudflavor/tap
```

Repo expectations:

- the tap repo stays public at `cloudflavor/homebrew-tap`
- the formula lives at `Formula/opal.rb`
- Homebrew reads the default branch of that repo

Good first test after a tap push:

```bash
brew untap cloudflavor/tap || true
brew tap cloudflavor/tap
brew install cloudflavor/tap/opal
```

## Install from a local checkout

```bash
cargo install --path .
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
