# Quick Start

1. **Install**
   ```bash
   cargo install --path .
   ```
   Opal requires Docker, Podman, nerdctl, or the sandbox runtime (Apple `srt`) to execute jobs.

2. **Prepare a workspace**
   - Place your project in a directory containing `.gitlab-ci.yml`.
   - Optional: add `.opal/env` with key-value files for secrets and set executable scripts under `.opal/hooks`.

3. **Run the pipeline**
   ```bash
   opal run --pipeline .gitlab-ci.yml --workdir .
   ```
   Use `--engine auto` (default) to let Opal detect which container runtime is available, or pass `--engine docker`, `podman`, `nerdctl`, or `sandbox`.

4. **Drive the UI**
   - Tabs show each job’s status (pending, running, waiting, success, failure).
   - The left column lists run history; use `↑/↓` to inspect past results.
   - Press `?` at any time to open the contextual help overlay. From there you can open these Markdown docs with `1-9` or `←/→`.

5. **Preview the DAG**
   ```bash
   opal plan --pipeline .gitlab-ci.yml --workdir .
   ```
   `opal plan` evaluates rules/filters and prints every stage, job, dependency, and manual gate so you can verify the DAG before any containers start.

6. **Inspect results**
   - Highlight a job and press `o` to open its log in your pager (`$PAGER`, default `less -R`).
   - Artifacts are stored in `$OPAL_HOME/<pipeline>/<job>/artifacts/` (default `~/.opal/<pipeline>/<job>/artifacts/`).

## Tips

- Pass `--max-parallel-jobs N` to increase concurrency.
- Use `--env APP_*` (repeatable) to forward host environment variables into the execution sandbox.
- Use `opal plan` regularly to confirm rules, manual gates, and artifact flows before you launch a run.
