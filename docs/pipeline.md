# Pipeline Model

This document describes how Opal interprets `.gitlab-ci.yml` and schedules jobs locally.

## Parsing

- Supported `include:` directives are resolved recursively from the local filesystem. Opal currently handles string paths plus `local:`, `file:`, and `files:` include entries.
- `default.*` values (scripts, variables, image) merge into every job unless explicitly overridden.
- Hidden/template jobs (names beginning with `.`) may be referenced via `extends`. Cycles are detected and reported.
- `workflow:rules`, `rules`, `only`, and `except` are partially supported. Unsupported keywords are logged so you know which GitLab features still need work.

## Graph construction

- Each job becomes a node in a DAG.
- Dependencies come from `needs:` (preferred) or the implicit stage ordering (`stages:` list).
- Parallel jobs (`parallel:<count>` or `parallel:matrix`) expand into individual job instances so you can examine each variant independently.

## Scheduling

- The executor keeps a ready queue and launches up to `--max-parallel-jobs` at a time.
- Jobs run in containers using the selected engine:
  - `docker`, `podman`, `nerdctl`, or `container` for OCI runtimes.
  - `sandbox` for the host-based Sandbox Runtime (`srt`) on macOS.
  - `auto` picks a sane default for the current platform.
- Manual jobs (`when: manual`) appear in the UI with `m` to start and `x` to cancel.
- When a job fails, downstream jobs that depend on it are cancelled, and no new work starts (`fail-fast` semantics). Use `r` to restart a failed job after fixing the issue.

## Artifacts & logs

- Each job’s filesystem writes go into `$OPAL_HOME/<pipeline>/<job>/artifacts/` (default `~/.opal/<pipeline>/<job>/artifacts/`).
- Declared `artifacts.paths` are copied into that directory and can be consumed by downstream jobs that request `needs: { artifacts: true }`.
- Logs stream to `$OPAL_HOME/<pipeline>/<job>/logs/<job>.log` for long-term archiving. The TUI also keeps an in-memory buffer and highlights diff-like lines (`+`/`-`).

## Customization

### Forwarding host env vars

- Use `--env GLOB` (repeat) to forward selected host environment variables into every job. The glob uses [`globset`](https://docs.rs/globset/latest/globset/) syntax, so `*` and `?` work the way they do in shells:

  ```bash
  opal run --env CI_* --env 'AWS_ACCESS_KEY_ID' --env 'APP_??'
  ```

  The example above forwards everything starting with `CI_`, both AWS credentials, and any `APP_` var with exactly two characters after the underscore. Patterns are evaluated against the host’s environment, and the matches are injected before job-level variables, so jobs can override them if needed.

  Repeat `--env` for each glob you need. Use quotes when your pattern includes characters your shell might expand (e.g., `?`).

### Repository secrets (`.opal/env`)

- Add plain files under `.opal/env` in your repo; the filename becomes the secret name. For example:

  ```
  .opal/env/
  ├─ GITLAB_TOKEN        # contains the token text
  └─ MY_CERT.crt         # binary cert, can be any name
  ```

- During a run Opal:
  - Copies the whole directory into the container at `.opal/secrets/…`, mounted read-only.
  - Sets `$GITLAB_TOKEN` to the file contents (if UTF‑8) and `$GITLAB_TOKEN_FILE=.opal/secrets/GITLAB_TOKEN`.
  - Always sets `$<NAME>_FILE` so you can read binary data even when the value isn’t UTF‑8.
  - Masks the plaintext values in logs (anything matching the file contents is replaced with `[MASKED]` before printing).

  This mirrors GitLab’s `_FILE` behavior, so jobs that already expect `_FILE` env vars work unchanged. Keep `.opal/env` out of version control (the default `.gitignore` already ignores it).

- Pass `--base-image` to supply a default container when jobs do not specify one.
- Pass `--base-image` to supply a default container when jobs do not specify one.

### Tracing job scripts

- Use `opal run --trace-scripts …` when you want every job to echo its commands as they execute. The flag makes Opal write each generated script with `set -x`, so you will see the shell-expanded command stream (`+ cargo fmt`, etc.) in the log.
- Alternatively set `OPAL_DEBUG=1` in the environment to enable the same behavior without touching CLI flags (useful when scripting or when you need trace logs globally).
- The tracing flag stacks with any verbosity coming from the job itself; Opal still forces `set -eu` so jobs fail fast while showing the debug output.

## Planning pipelines

Run `opal plan` when you want to inspect the DAG without touching containers. The command parses `.gitlab-ci.yml`, evaluates workflow/`rules`, and prints each stage with:

- Job order plus dependency list (`depends on` shows implicit stage ordering when no explicit `needs` exist).
- Manual/delayed gates, retry counts, timeouts, and whether a job may fail without stopping the pipeline.
- Artifact paths, environments, and resource groups.

It is the fastest way to understand why a job is (or is not) scheduled for the current branch/tag, and it surfaces external `needs` so you can adjust infrastructure before running the pipeline for real.

This model mirrors GitLab closely while remaining deterministic and debuggable on a single machine. When in doubt, compare the DAG produced by `opal plan` with GitLab’s pipeline graph to ensure parity.
