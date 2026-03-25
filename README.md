# opal

Opal is a terminal-first GitLab pipeline runner for local debugging. It parses `.gitlab-ci.yml`, evaluates a practical local-runner subset of GitLab filters/rules, executes jobs in local containers, and provides a keyboard-driven UI for run history, logs, artifacts, and docs.

## Features

- `opal run` executes a local-runner subset of GitLab pipelines (including `rules`, `workflow:rules`, `needs`, `dependencies`, artifacts, cache, services, and matrix jobs).
- `opal plan` prints a dry-run execution plan without starting containers.
- `opal view` opens the history/log browser for previous runs.
- Ratatui UI with help overlays, embedded Markdown docs, and pager integration for plans/logs/files.
- Multiple engines: `docker`, `podman`, `nerdctl`, Apple `container`, and `orbstack`.

## Quick Start

```bash
cargo install --path .
opal run --workdir . --pipeline .gitlab-ci.yml
```

Preview the DAG without execution:

```bash
opal plan --workdir . --pipeline .gitlab-ci.yml
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

Current fixture files:

- `needs-and-artifacts.gitlab-ci.yml`
- `rules-playground.gitlab-ci.yml`
- `includes-and-extends.gitlab-ci.yml`
- `resources-and-services.gitlab-ci.yml`
- `services-readiness-failure.gitlab-ci.yml`
- `cache-policies.gitlab-ci.yml`
- `cache-key-files.gitlab-ci.yml`
- `cache-fallback.gitlab-ci.yml`
- `filters.gitlab-ci.yml`
- `environments.gitlab-ci.yml`
- `secret-masking.gitlab-ci.yml`
- `tag-ambiguity.gitlab-ci.yml`

Run the suite with representative env permutations:

```bash
./scripts/test-pipelines.sh
```

The script writes logs under `tests-temp/test-pipeline-logs/`. By default it uses the installed `opal` from `PATH`; you can override that and other defaults with `OPAL_BIN`, `OPAL_TEST_COMMAND`, and `OPAL_TEST_ARGS`.

## Documentation

See `docs/` for deeper references:

- `docs/quickstart.md`
- `docs/ui.md`
- `docs/plan.md`
- `docs/pipeline.md`
- `docs/gitlab-parity.md`

Use `docs/gitlab-parity.md` for the exact supported surface and known divergences from GitLab Runner/GitLab CI.

The `docs/` directory is embedded into the TUI help viewer at build time.

## Releasing

`./scripts/build-release-artifacts.sh` packages:

- `aarch64-apple-silicon` (built locally on macOS hosts)
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-gnu`

Requirements:

- Run from a tagged commit (or set `CI_COMMIT_TAG`).
- On macOS, install Apple `container` CLI (or Docker/Podman/NerdCTL) for Linux target builds.
- On Linux hosts, only Linux artifacts are produced.

Artifacts and per-platform checksum notes are written to `./releases/`.

## License

Licensed under the Apache License, Version 2.0. See `LICENSE`.
