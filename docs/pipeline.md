# Pipeline Model

This document describes how Opal interprets `.gitlab-ci.yml` and schedules jobs locally.

## Parsing

- All `include:` directives are resolved recursively. Local `path:` entries read from disk; remote/project references reuse GitLab’s semantics when possible.
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

- Each job’s filesystem writes go into `.opal/<run-id>/artifacts/<job-slug>/`.
- Declared `artifacts.paths` are copied into that directory and can be consumed by downstream jobs that request `needs: { artifacts: true }`.
- Logs stream to `.opal/logs/<run-id>/<job>.log` for long-term archiving. The TUI also keeps an in-memory buffer and highlights diff-like lines (`+`/`-`).

## Customization

- Use `--env GLOB` (repeat) to forward selected host environment variables into every job.
- Provide secrets in `.opal/env/NAME` or `.opal/env/NAME_FILE`. Opal mounts them read-only and populates `$NAME` / `$NAME_FILE`.
- Pass `--base-image` to supply a default container when jobs do not specify one.

## Planning pipelines

Run `opal plan` when you want to inspect the DAG without touching containers. The command parses `.gitlab-ci.yml`, evaluates workflow/`rules`, and prints each stage with:

- Job order plus dependency list (`depends on` shows implicit stage ordering when no explicit `needs` exist).
- Manual/delayed gates, retry counts, timeouts, and whether a job may fail without stopping the pipeline.
- Artifact paths, environments, and resource groups.

It is the fastest way to understand why a job is (or is not) scheduled for the current branch/tag, and it surfaces external `needs` so you can adjust infrastructure before running the pipeline for real.

This model mirrors GitLab closely while remaining deterministic and debuggable on a single machine. When in doubt, compare the DAG produced by `opal plan` with GitLab’s pipeline graph to ensure parity.
