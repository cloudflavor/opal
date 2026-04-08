# Quick Start

## Install

```bash
cargo install --path crates/opal
```

This installs the executable as `opal`.

Opal requires Docker, Podman, Apple `container`, or OrbStack for the supported local engine set. `nerdctl` remains available as a Linux-oriented option when the underlying environment is directly usable.

Opal wraps those local engine CLIs; it does not bundle its own container runtime. Make sure the engine you want to use is already installed and available on your `PATH`.

For the Apple `container` engine, use the official project:

```text
https://github.com/apple/container
```

If you are installing from a local checkout while developing Opal itself, use:

```bash
cargo install --path crates/opal
```

## Prepare A Workspace

- Place your project in a directory containing `.gitlab-ci.yml`.
- Optional: add `.opal/env` with key-value files for secrets (each filename becomes a `$NAME` and `$NAME_FILE` inside containers).
- Optional: use `--env GLOB` (repeatable) to forward selected host variables into jobs; see `docs/pipeline.md` for the exact glob behavior and examples.

## Run The Pipeline

```bash
opal run --pipeline .gitlab-ci.yml --workdir .
```

Use `--engine auto` (default) to let Opal detect which container runtime is available, or pass `--engine docker`, `podman`, `nerdctl`, `container`, or `orbstack`.
On macOS, the RC-supported local engine set is `container`, `docker`, `orbstack`, and `podman`.
Add `--job <name>` (repeatable) when you want to run only selected jobs plus their required upstream dependencies.

Default engine selection:

- macOS: `auto` uses Apple `container`
- Linux: `auto` uses `podman`

You can override the `auto` default in config with:

```toml
[engine]
default = "docker"
```

## Run Without The TUI

```bash
opal run --no-tui
```

Use this mode when you want plain terminal output instead of the interactive interface, for example in scripts, local CI-style checks, or when sharing a terminal recording.

If multiple jobs run in parallel, their output can still interleave in `--no-tui` mode. Opal prefixes each streamed line with the job name so you can still tell which job emitted it.

## Drive The UI

- Tabs show each job’s status (pending, running, waiting, success, failure).
- The left column lists run history; use `↑/↓` to inspect past results.
- Press `?` at any time to open the contextual help overlay. From there you can open these Markdown docs with `1-9` or `←/→`.

## Preview The DAG

```bash
opal plan --pipeline .gitlab-ci.yml --workdir .
```

`opal plan` evaluates rules/filters and prints every stage, job, dependency, and manual gate so you can verify the DAG before any containers start.

## Inspect Results

- Highlight a job and press `o` to open its log in your pager (`$PAGER`, default `less -R`).
- Artifacts are stored in `$OPAL_HOME/<run-id>/<job>/artifacts/` (default `~/.local/share/opal/<run-id>/<job>/artifacts/`).

## Tips

- Pass `--max-parallel-jobs N` to increase concurrency.
- Use `--env APP_*` (repeatable) to forward host environment variables into the execution sandbox.
- Use `opal plan` regularly to confirm rules, manual gates, and artifact flows before you launch a run.
