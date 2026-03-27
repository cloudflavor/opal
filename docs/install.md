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

Placeholder examples:

```bash
# macOS Apple Silicon
curl -L <placeholder> | tar xz

# Linux ARM64
curl -L <placeholder> | tar xz

# Linux AMD64
curl -L <placeholder> | tar xz
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

## Verify the install

```bash
opal --version
opal plan --no-pager
```
