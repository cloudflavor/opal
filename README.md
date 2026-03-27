# opal

[![Docs](https://img.shields.io/badge/docs-opal.cloudflavor.io-0ea5e9)](https://opal.cloudflavor.io)

Opal is a terminal-first GitLab pipeline runner for local debugging. It parses `.gitlab-ci.yml`, evaluates a practical local-runner subset of GitLab filters/rules, executes jobs in local containers, and provides a keyboard-driven UI for run history, logs, artifacts, and docs.

## Demo

### Opal Run

[![asciicast](https://asciinema.org/a/LxDIEb87AyDtDM5c.svg)](https://asciinema.org/a/LxDIEb87AyDtDM5c)

### Opal Plan

[![asciicast](https://asciinema.org/a/6rP1p3H4vtA7Orr8.svg)](https://asciinema.org/a/6rP1p3H4vtA7Orr8)

### Opal Run --no-tui

[![asciicast](https://asciinema.org/a/2Q2mFyHkwXMoI9e0.svg)](https://asciinema.org/a/2Q2mFyHkwXMoI9e0)

### Opal View

[![asciicast](https://asciinema.org/a/3eTpVFphkhQKDZB9.svg)](https://asciinema.org/a/3eTpVFphkhQKDZB9)

## Features

- `opal run` executes a practical local-runner subset of GitLab pipelines, including `rules`, `workflow:rules`, `needs`, `dependencies`, artifacts, cache, services, and matrix jobs.
- `opal plan` prints the evaluated DAG without starting containers.
- `opal view` opens the history/log browser for previous runs.
- The TUI includes embedded markdown docs, help overlays, pager integration, and history/resource browsing. Press `?` inside the TUI to open the built-in documentation viewer.
- GitLab-style predefined job metadata is injected into job environments, including `CI_JOB_NAME`, `CI_JOB_NAME_SLUG`, `CI_JOB_STAGE`, `CI_PROJECT_DIR`, and `CI_PIPELINE_ID`.
- Supported local engines: `docker`, `podman`, Apple `container`, and `orbstack`.
- `nerdctl` remains available as a Linux-oriented engine option when the underlying `containerd` environment is directly usable.

## Quick Start

```bash
cargo install opal
opal run
```

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

Preview the DAG without execution:

```bash
opal plan --workdir . --pipeline .gitlab-ci.yml
```

Limit planning or execution to specific jobs plus their required upstream dependencies:

```bash
opal plan --workdir . --pipeline .gitlab-ci.yml --job package-linux
opal run --workdir . --pipeline .gitlab-ci.yml --job lint
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

## Releasing

Build release artifacts with:

```bash
./scripts/build-release-artifacts.sh
```

Artifacts are written under `releases/`.

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
- `docs/plan.md`
- `docs/pipeline.md`
- `docs/gitlab-parity.md`

Release-candidate preparation notes live outside the embedded docs set in `release/rc-checklist.md`.

Use `docs/gitlab-parity.md` for the exact supported surface and known divergences from GitLab Runner/GitLab CI.
For exact runtime usage details, especially host env forwarding and repository secrets, see `docs/pipeline.md`.

The `docs/` directory is embedded into the Opal binary at build time and can be opened from the TUI with `?`.

## License

Licensed under the Apache License, Version 2.0. See `LICENSE`.
