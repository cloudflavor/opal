# opal

[![Docs](https://img.shields.io/badge/docs-opal.cloudflavor.io-0ea5e9)](https://opal.cloudflavor.io)

Opal is a terminal-first, AI-capable GitLab pipeline runner for local debugging. It parses `.gitlab-ci.yml`, evaluates a practical local-runner subset of GitLab filters/rules, executes jobs in local containers, and provides a keyboard-driven UI for run history, logs, artifacts, docs, and job analysis.

## Demo

### Opal Run

[![asciicast](https://asciinema.org/a/LxDIEb87AyDtDM5c.svg)](https://asciinema.org/a/LxDIEb87AyDtDM5c)

### Opal Plan

[![asciicast](https://asciinema.org/a/6rP1p3H4vtA7Orr8.svg)](https://asciinema.org/a/6rP1p3H4vtA7Orr8)

### Opal Run --no-tui

[![asciicast](https://asciinema.org/a/2Q2mFyHkwXMoI9e0.svg)](https://asciinema.org/a/2Q2mFyHkwXMoI9e0)

### Opal View

[![asciicast](https://asciinema.org/a/3eTpVFphkhQKDZB9.svg)](https://asciinema.org/a/3eTpVFphkhQKDZB9)

### Opal AI Troubleshooting · Codex

[![asciicast](https://asciinema.org/a/876637.svg)](https://asciinema.org/a/876637)

### Opal AI Troubleshooting · Ollama

[![asciicast](https://asciinema.org/a/876581.svg)](https://asciinema.org/a/876581)

## Features

- `opal run` executes a practical local-runner subset of GitLab pipelines, including `rules`, `workflow:rules`, `needs`, `dependencies`, artifacts, cache, services, and matrix jobs.
- `opal plan` prints the evaluated DAG without starting containers.
- `opal view` opens the history/log browser for previous runs.
- The TUI can analyze a selected job with AI backends, preview the exact rendered prompt, and keep troubleshooting inside the terminal.
- The TUI includes embedded markdown docs, help overlays, pager integration, and history/resource browsing. Press `?` inside the TUI to open the built-in documentation viewer.
- GitLab-style predefined job metadata is injected into job environments, including `CI_JOB_NAME`, `CI_JOB_NAME_SLUG`, `CI_JOB_STAGE`, `CI_PROJECT_DIR`, and `CI_PIPELINE_ID`.
- Supported local engines: `docker`, `podman`, Apple `container`, and `orbstack`.
- `nerdctl` remains available as a Linux-oriented engine option when the underlying `containerd` environment is directly usable.
- On macOS, Apple `container` is a strong default for Opal because it runs each container in its own lightweight VM instead of routing all containers through one shared Linux VM, which improves per-job isolation while keeping a lightweight local workflow.

## Quick Start

```bash
cargo install opal-cli
opal run
```

This installs the executable as `opal` on your system.

For a local checkout during development, use:

```bash
cargo install --path .
```

Common entry points:

```bash
opal run
opal run --no-tui
opal plan
opal view
```

When `opal plan` runs in an interactive terminal, it now opens in your pager by default. Use `--no-pager` to print directly or `--json` for machine-readable output.

By default, Opal expects `.gitlab-ci.yml` in the current working directory and prepares each job from a snapshot of that current working tree.

Default engine selection:

- macOS: `auto` uses Apple `container`
- Linux: `auto` uses `podman`

You can override the `auto` default in config with:

```toml
[engine]
default = "docker"
```

Preview the DAG without execution:

```bash
opal plan --workdir . --pipeline .gitlab-ci.yml
```

Limit planning or execution to specific jobs plus their required upstream dependencies:

```bash
opal plan --workdir . --pipeline .gitlab-ci.yml --job package-linux
opal run --job rust-checks
```

The full user-facing command surface, engine behavior, and runtime usage now live in the docs set rather than in this README.

## Testing

Run the fixture suite with:

```bash
./scripts/test-pipelines.sh
```

The script writes logs under `tests-temp/test-pipeline-logs/`.

Useful overrides:

- `OPAL_BIN`
- `OPAL_TEST_COMMAND`
- `OPAL_TEST_ARGS`

Example:

```bash
OPAL_BIN=target/debug/opal OPAL_TEST_ARGS='--no-tui --engine docker' ./scripts/test-pipelines.sh
```

For the current fixture matrix and scenario descriptions, see `pipelines/tests/README.md`.

## AI troubleshooting

Opal can analyze a selected job directly from the TUI.

- `a` starts analysis for the selected job and toggles the analysis view once it exists
- `A` previews the exact rendered prompt that Opal will send
- `o` opens the current log or analysis view in your pager

This also works when you load a past job in `opal view`.

Current backends:

- `ollama`
- `codex`

See:

- `docs/ai.md` for usage, current behavior, and provider notes
- `docs/ai-config.md` for backend selection, prompt files, and AI configuration

## Releasing

Build release artifacts with:

```bash
bash ./scripts/build-release-artifacts.sh
```

Artifacts are written under `releases/`.

Tag-driven release publishing expectations:

- release tags must match `Cargo.toml`'s package version, allowing an optional leading `v`
- plain `opal run` does not turn into a tag pipeline just because `HEAD` is tagged; set `CI_COMMIT_TAG` or `GIT_COMMIT_TAG` explicitly when you want local tag-pipeline behavior
- Linux release artifacts are split into separate `arm64` and `amd64` release jobs so each target runs in its own matching container image platform
- `CARGO_REGISTRY_TOKEN` enables automatic crates.io publishing from the tag pipeline
- `GITHUB_TOKEN` plus `GITHUB_REPOSITORY=owner/repo` enables automatic GitHub release publishing with the built tarballs and release notes
- if `release/notes/<tag>.md` or `release/notes/<version>.md` exists, that file becomes the GitHub release body; otherwise the release job composes notes from the generated per-platform archive summaries

To run the tag pipeline locally:

```bash
CI_COMMIT_TAG=v0.1.0-rc3 opal run --no-tui
```

Package validation commands:

```bash
cargo package --list
cargo publish --dry-run
```

Release-candidate preparation notes live in `release/rc-checklist.md`.

## Documentation

Read the hosted docs at:

- `https://opal.cloudflavor.io`

Key references in the repo docs set:

- `docs/quickstart.md`
- `docs/cli.md`
- `docs/ui.md`
- `docs/ai.md`
- `docs/plan.md`
- `docs/pipeline.md`
- `docs/gitlab-parity.md`

Release-candidate preparation notes live outside the embedded docs set in `release/rc-checklist.md`.

Use `docs/gitlab-parity.md` for the exact supported surface and known divergences from GitLab Runner/GitLab CI.
For exact runtime usage details, especially host env forwarding and repository secrets, see `docs/pipeline.md`.

The `docs/` directory is embedded into the Opal binary at build time and can be opened from the TUI with `?`.

## License

Licensed under the Apache License, Version 2.0. See `LICENSE`.
