# opal

Opal is a terminal-first GitLab pipeline runner for local debugging. It parses `.gitlab-ci.yml`, evaluates a practical local-runner subset of GitLab filters/rules, executes jobs in local containers, and provides a keyboard-driven UI for run history, logs, artifacts, and docs.

## Features

- `opal run` executes a local-runner subset of GitLab pipelines (including `rules`, `workflow:rules`, `needs`, `dependencies`, artifacts, cache, services, and matrix jobs).
- `opal plan` prints a dry-run execution plan without starting containers.
- `opal view` opens the history/log browser for previous runs.
- Ratatui UI with help overlays, embedded Markdown docs, and pager integration for plans/logs/files.
- GitLab-style predefined job metadata is injected into job environments, including `CI_JOB_NAME`, `CI_JOB_NAME_SLUG`, `CI_JOB_STAGE`, `CI_PROJECT_DIR`, and `CI_PIPELINE_ID`.
- Supported local engines: `docker`, `podman`, Apple `container`, and `orbstack`.
- `nerdctl` remains available as a Linux-oriented engine option when the underlying `containerd` environment is actually local and directly usable.

## Quick Start

```bash
cargo install --path .
opal run --workdir . --pipeline .gitlab-ci.yml
```

Preview the DAG without execution:

```bash
opal plan --workdir . --pipeline .gitlab-ci.yml
```

Limit planning or execution to specific jobs plus their required upstream dependencies:

```bash
opal plan --workdir . --pipeline .gitlab-ci.yml --job package-linux
opal run --workdir . --pipeline .gitlab-ci.yml --job lint
```

Open the stored history/log UI from previous runs:

```bash
opal view
```

Useful `run` flags:

- `--engine auto|container|docker|podman|nerdctl|orbstack`
- `--max-parallel-jobs <N>`
- `-E, --env <GLOB>` (repeatable, for host env passthrough)
- `--trace-scripts` (enables shell `set -x`)
- `--no-tui`
- `--gitlab-token` and `--gitlab-base-url` (for cross-project `needs:project` artifact downloads; requires network access)

Engine auto-selection behavior currently is:

- macOS: prefer `orbstack` when detected, otherwise use Apple `container`.
- Linux: use `docker`.

Release-candidate supported local engines are:

- macOS: `container`, `docker`, `orbstack`, `podman`
- Linux: `docker`, `podman`, `nerdctl`

## Runtime and Config

- Runtime data is stored under `$OPAL_HOME` (default `~/.opal`):
  - per-run session directories (`logs/`, scripts, artifacts)
  - `cache/`
  - `history.json`
- Config is loaded from these paths (if present), then merged in order:
  - `<workdir>/.opal/config.toml`
  - `$OPAL_HOME/config.toml`
  - `$XDG_CONFIG_HOME/opal/config.toml` (platform default when unset)

See `docs/config.md` for full config keys (engine tunables and registry auth).

## Test Pipelines

Use fixtures under `pipelines/tests/` to regression-test parser/planner/executor behavior with CI-like env vars.

Example:

```bash
CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push \
  opal run --pipeline pipelines/tests/needs-and-artifacts.gitlab-ci.yml
```

For the current fixture matrix and scenario descriptions, see `pipelines/tests/README.md`.

Run the suite with representative env permutations:

```bash
./scripts/test-pipelines.sh
```

The script writes logs under `tests-temp/test-pipeline-logs/`. By default it uses the installed `opal` from `PATH`; you can override that and other defaults with `OPAL_BIN`, `OPAL_TEST_COMMAND`, and `OPAL_TEST_ARGS`.

## Documentation

See `docs/` for deeper references:

- `docs/quickstart.md`
- `docs/cli.md`
- `docs/ui.md`
- `docs/plan.md`
- `docs/pipeline.md`
- `docs/gitlab-parity.md`

Release-candidate preparation notes live outside the embedded docs set in `release/rc-checklist.md`.

Use `docs/gitlab-parity.md` for the exact supported surface and known divergences from GitLab Runner/GitLab CI.
For exact runtime usage details, especially host env forwarding and repository secrets, see `docs/pipeline.md`.

The `docs/` directory is embedded into the TUI help viewer at build time.

## Releasing

`./scripts/build-release-artifacts.sh` packages:

- `aarch64-apple-silicon` (built locally on macOS hosts)
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-gnu`

Requirements:

- Run from a tagged commit (or set `CI_COMMIT_TAG`).
- On macOS, use one of the supported local engines for release artifact work: Apple `container`, Docker, OrbStack, or Podman.
- On Linux hosts, only Linux artifacts are produced.

Artifacts and per-platform checksum notes are written to `./releases/`.

## License

Licensed under the Apache License, Version 2.0. See `LICENSE`.
