# Quick Start

1. **Install**
   ```bash
   cargo install --path .
   ```
   Opal requires Docker, Podman, Apple `container`, or OrbStack for the supported local engine set. `nerdctl` remains available as a Linux-oriented option when the underlying environment is directly usable.

2. **Prepare a workspace**
   - Place your project in a directory containing `.gitlab-ci.yml`.
   - Optional: add `.opal/env` with key-value files for secrets (each filename becomes a `$NAME` and `$NAME_FILE` inside containers) and set executable scripts under `.opal/hooks`.
   - Optional: use `--env GLOB` (repeatable) to forward selected host variables into jobs; see `docs/pipeline.md` for the exact glob behavior and examples.

3. **Run the pipeline**
   ```bash
   opal run --pipeline .gitlab-ci.yml --workdir .
   ```
   Use `--engine auto` (default) to let Opal detect which container runtime is available, or pass `--engine docker`, `podman`, `nerdctl`, `container`, or `orbstack`.
   On macOS, the RC-supported local engine set is `container`, `docker`, `orbstack`, and `podman`.
   Add `--job <name>` (repeatable) when you want to run only selected jobs plus their required upstream dependencies.

4. **Drive the UI**
   - Tabs show each job’s status (pending, running, waiting, success, failure).
   - The left column lists run history; use `↑/↓` to inspect past results.
   - Press `?` at any time to open the contextual help overlay. From there you can open these Markdown docs with `1-9` or `←/→`.


   ![`opal run` in Ghostty](assets/opal-run-window.png)

5. **Preview the DAG**
   ```bash
   opal plan --pipeline .gitlab-ci.yml --workdir .
   ```
   `opal plan` evaluates rules/filters and prints every stage, job, dependency, and manual gate so you can verify the DAG before any containers start.

6. **Inspect results**
   - Highlight a job and press `o` to open its log in your pager (`$PAGER`, default `less -R`).
   - Artifacts are stored in `$OPAL_HOME/<run-id>/<job>/artifacts/` (default `~/.opal/<run-id>/<job>/artifacts/`).

## Tips

- Pass `--max-parallel-jobs N` to increase concurrency.
- Use `--env APP_*` (repeatable) to forward host environment variables into the execution sandbox.
- Use `opal plan` regularly to confirm rules, manual gates, and artifact flows before you launch a run.
