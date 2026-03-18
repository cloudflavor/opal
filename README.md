# opal

Opal is a terminal-first GitLab runner that lets you inspect plans, stream logs, and dogfood pipelines locally. It embeds your documentation, renders a detailed history pane, and provides enough context to debug failing jobs without leaving the console.

## Features

- Rich Ratatui interface with help overlays, searchable history, and log previews.
- `opal run` to execute the full pipeline against your `.gitlab-ci.yml`.
- `opal plan` for a dry-run preview that respects `rules`, `needs`, and artifacts.
- Directory and artifact viewers so you can dig into build outputs immediately.
- Pager integration for the pipeline plan and embedded Markdown docs.

## Usage

```bash
cargo install --path .
opal run --pipeline .gitlab-ci.yml
```

See the docs in `docs/` (e.g. `docs/plan.md`, `docs/ui.md`) for deeper walkthroughs, shortcuts, and design notes.

## Test Pipelines

Use the fixtures under `pipelines/tests/` to reproduce GitLab behaviors locally. Each file is a synthetic `.gitlab-ci.yml` that stresses a specific feature set (rules, needs/dependencies, optional jobs, `!reference`, manual/delayed gates, etc.). Run them with:

```bash
CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push opal run --pipeline pipelines/tests/needs-and-artifacts.gitlab-ci.yml
```

Adjust CI variables (`CI_COMMIT_TAG`, `RUN_DELAYED`, `ENABLE_OPTIONAL`, `FORCE_DOCS`, etc.) or touch files under `docs/` to exercise different code paths before/after making engine changes.

Available fixtures:

- `needs-and-artifacts.gitlab-ci.yml`
- `rules-playground.gitlab-ci.yml`
- `includes-and-extends.gitlab-ci.yml`
- `resources-and-services.gitlab-ci.yml`
- `filters.gitlab-ci.yml`
- `environments.gitlab-ci.yml`

Run all fixtures (with representative CI env permutations) via `./scripts/test-pipelines.sh`. The script writes logs and scratch outputs under `tests-temp/`, which is ignored by git. Override the binary or extra flags with `OPAL_BIN=/path/to/opal` or `OPAL_TEST_ARGS="--no-tui --max-parallel-jobs 4"` if needed.

## Releasing

`./scripts/build-release-artifacts.sh` packages the three supported targets:

- `aarch64-apple-silicon` (built locally on macOS hosts)
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-gnu`

Requirements:

- Run the script from a tagged commit (or set `CI_COMMIT_TAG`); it exits otherwise.
- On macOS, install Apple’s `container` CLI (or Docker/Podman/NerdCTL) so the Linux targets can be compiled inside containers while the Apple Silicon binary is built natively.
- On Linux CI runners, only the Linux archives are produced; macOS hosts should be used for full releases.

Artifacts and per-platform checksum notes are written to `./releases/`, matching the pipeline’s `release-artifacts` job.

## License

Licensed under the Apache License, Version 2.0. See the `LICENSE` file for details.
