# Pipeline Model

This document describes how Opal interprets `.gitlab-ci.yml` and schedules jobs locally.

## Parsing

- Supported `include:` directives are resolved recursively from the local filesystem. Opal currently handles string paths plus `local:`, `file:`, and `files:` include entries.
  Local include paths are resolved from the repository root, local include wildcards such as `configs/*.yml` are supported, include paths can use parse-time environment expansion, and included files must be `.yml` or `.yaml`. Plain `file:` / `files:` entries are still local-only conveniences rather than full GitLab `include:project` semantics. `include:project` is available as a partial local approximation when `--gitlab-token` is configured, including nested direct local includes inside the fetched project; `remote:`, `template:`, and `component:` still fail explicitly.
- `default.*` values merge into jobs for the subset Opal models today: `image`, `before_script`, `after_script`, `variables`, `cache`, `services`, `timeout`, `retry`, and `interruptible`. For `retry`, Opal models `max`, `when`, and `exit_codes` for local rerun decisions.
- `inherit:default` can now disable or selectively retain the modeled default-key subset: `image`, `before_script`, `after_script`, `cache`, `services`, `timeout`, `retry`, and `interruptible`.
- `image` supports string form, `image.name`, `image.entrypoint`, and `image:docker:platform` / `image:docker:user` for local engines that can express those options.
- `services` supports string form plus mapping entries with `name`, `alias`, `entrypoint`, `command`, `variables`, and `services:docker:platform` / `services:docker:user`.
- Job environments include GitLab-style predefined metadata such as `CI_JOB_NAME`, `CI_JOB_NAME_SLUG`, `CI_JOB_STAGE`, `CI_PROJECT_DIR`, and `CI_PIPELINE_ID`.
- `[[jobs]]` runtime overrides from `.opal/config.toml` can target exact job names to adjust local engine behavior like architecture selection or Linux capability flags without editing the pipeline itself.
- Hidden/template jobs (names beginning with `.`) may be referenced via `extends`. Cycles are detected and reported.
- `workflow:rules`, `rules`, `only`, and `except` are partially supported. See `docs/gitlab-parity.md` for the exact supported forms and known divergences.

## Graph construction

- Each job becomes a node in a DAG.
- Scheduler dependencies come from explicit `needs:` or the implicit stage ordering (`stages:` list) when `needs:` is not present.
- `dependencies:` affects which artifacts are mounted/restored for a job; it does not create scheduler edges.
- `needs:project` can download cross-project artifacts when GitLab credentials are configured.
- Parallel jobs (`parallel:<count>` or `parallel:matrix`) expand into individual job instances so you can examine each variant independently.

## Scheduling

- The executor keeps a ready queue and launches up to `--max-parallel-jobs` at a time.
- Jobs run in containers using the selected engine:
  - `docker`, `podman`, `container`, or `orbstack` for the supported local engine set.
  - `nerdctl` remains available as a Linux-oriented option when the underlying `containerd` environment is directly usable.
  - `auto` picks a sane default for the current platform.
- Job services start as sibling containers on a per-job network, and Opal performs a readiness gate before running the job script when service inspection is available. On Apple’s `container` engine, Opal now fails fast if the underlying `container network create` call stalls instead of hanging indefinitely.
- Manual jobs (`when: manual`) appear in the UI and can be started interactively.
- `resource_group` serializes matching jobs within a local run.
- When a job fails, downstream jobs that depend on it are cancelled, and no new work starts (`fail-fast` semantics). Use `r` to restart a failed job after fixing the issue.

This is intentionally a local-runner approximation, not a full reproduction of GitLab Runner orchestration semantics. In particular, service networking, `interruptible`, and cross-pipeline coordination remain partial.

## Artifacts & logs

- Each run gets a session directory under `$OPAL_HOME/<run-id>/` (default `~/.opal/<run-id>/`). Job artifacts are stored under `$OPAL_HOME/<run-id>/<job>/artifacts/`.
- Declared `artifacts.paths` are copied into that directory and can be consumed by downstream jobs that request `needs: { artifacts: true }`. `artifacts:untracked` is also collected.
- `dependencies:` can mount a narrower artifact subset from earlier jobs, and `needs:project` can fetch artifacts from GitLab when `--gitlab-token` is configured.
- Logs are archived under `$OPAL_HOME/<run-id>/logs/`. The TUI also keeps an in-memory buffer and highlights diff-like lines (`+`/`-`).

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
  - Copies the whole directory into the container at `/opal/secrets/…`, mounted read-only.
  - Sets `$GITLAB_TOKEN` to the file contents (if UTF‑8) and `$GITLAB_TOKEN_FILE=/opal/secrets/GITLAB_TOKEN`.
  - Always sets `$<NAME>_FILE` so you can read binary data even when the value isn’t UTF‑8.
  - Masks the plaintext values in logs (anything matching the file contents is replaced with `[MASKED]` before printing).

  This mirrors GitLab’s `_FILE` behavior, so jobs that already expect `_FILE` env vars work unchanged. Keep `.opal/env` out of version control (the default `.gitignore` already ignores it).

- Pass `--base-image` to supply a default container when jobs do not specify one.

### Tracing job scripts

- Use `opal run --trace-scripts …` when you want every job to echo its commands as they execute. The flag makes Opal write each generated script with `set -x`, so you will see the shell-expanded command stream (`+ cargo fmt`, etc.) in the log.
- Alternatively set `OPAL_DEBUG=1` in the environment to enable the same behavior without touching CLI flags (useful when scripting or when you need trace logs globally).
- The tracing flag stacks with any verbosity coming from the job itself; Opal still forces `set -eu` so jobs fail fast while showing the debug output.

## Planning pipelines

Run `opal plan` when you want to inspect the DAG without touching containers. The command parses `.gitlab-ci.yml`, evaluates top-level filters plus workflow/`rules`, and prints each stage with:

- Job order plus dependency list (`depends on` shows implicit stage ordering when no explicit `needs` exist).
- Manual/delayed gates, retry counts, timeouts, and whether a job may fail without stopping the pipeline.
- Artifact paths, environments, services, tags, and resource groups.

It is the fastest way to understand why a job is (or is not) scheduled for the current branch/tag, and it surfaces external `needs` so you can adjust credentials/infrastructure before running the pipeline for real.

Use `--job <name>` (repeatable) with either `opal plan` or `opal run` when you want to focus on one part of the pipeline. Opal keeps the selected jobs and automatically includes the required upstream dependency closure so the resulting subgraph remains runnable.

This model mirrors a useful local subset of GitLab while remaining deterministic and debuggable on a single machine. When in doubt, compare the DAG produced by `opal plan` with GitLab’s pipeline graph and consult `docs/gitlab-parity.md` for known differences.
