# Test Pipelines

This directory contains `.gitlab-ci.yml` snippets that exercise the trickier parts of GitLab’s pipeline syntax so we can regression-test Opal locally. Each file can be passed to `opal run --pipeline <file>` along with CI-style environment variables to mimic different runners, branches, or triggers.

## Available scenarios

- `needs-and-artifacts.gitlab-ci.yml` – Covers `workflow:rules`, `default.before_script/after_script`, `!reference`, artifact passing via `needs`, `needs:optional`, `dependencies`, artifact exclusions, untracked artifact capture, manual/tagged releases, delayed jobs, environments, and `parallel:matrix` builds.
- `rules-playground.gitlab-ci.yml` – Focuses on the `rules:` mini-language: `if`, `changes`, `exists`, `when: manual|delayed`, inline `allow_failure`, schedule-only behavior, and interaction with `workflow:rules`.
- `includes-and-extends.gitlab-ci.yml` – Exercises local `include:`, hidden/template jobs, `extends`, `inherit: { default: [...] }`, and shared variables (see `job_inherit_flags` in `src/gitlab/parser.rs`).
- `includes-parity.gitlab-ci.yml` – Exercises local include parity behavior: repository-root `include:local`, wildcard local includes, `include:files`, and parse-time environment expansion in include paths.
- `include-surface.gitlab-ci.yml` – Exercises additional local include forms: bare string include entries and singular `include:file` entries.
- `include-remote-unsupported.gitlab-ci.yml` / `include-template-unsupported.gitlab-ci.yml` / `include-component-unsupported.gitlab-ci.yml` – Ensure unsupported non-local include types fail explicitly.
- `resources-and-services.gitlab-ci.yml` – Validates caches, retries, timeouts, `interruptible`, `resource_group` locking, and job-specific `services` the way `src/gitlab/graph.rs` models them.
- `services-and-tags.gitlab-ci.yml` – Validates service string/mapping forms plus informational runner tags in planner output.
- `services-readiness-failure.gitlab-ci.yml` – Validates service readiness failure handling by starting a deliberately broken sidecar and expecting Opal to fail before job script execution.
- `cache-policies.gitlab-ci.yml` – Validates local cache restore/save semantics, especially `cache:policy: pull` behavior where jobs can write to restored cache contents without persisting those changes back to the shared cache.
- `cache-fallback.gitlab-ci.yml` – Validates `cache:fallback_keys` restore behavior by seeding a default-branch cache and then restoring it from a feature-branch run when the primary key is missing.
- `retry-parity.gitlab-ci.yml` – Validates retry reruns for both `retry:when: script_failure` and `retry:exit_codes`, using the mounted Opal session directory so the first attempt fails and the retry succeeds.
- `dotenv-reports.gitlab-ci.yml` – Validates `artifacts:reports:dotenv` propagation through both `needs` and `dependencies`, and verifies that `needs:artifacts: false` plus `dependencies: []` block those variables.
- `control-flow-parity.gitlab-ci.yml` – Validates numeric `parallel`, top-level `image`/`variables`, rule-scoped `variables`, and `when: on_failure` behavior.
- `rules-compare-to.gitlab-ci.yml` – Validates `rules:changes:compare_to` against a temporary git worktree created by the harness.
- `needs-surface.gitlab-ci.yml` – Validates `needs:artifacts: false` and matrix-targeted `needs.parallel` planner behavior.
- `top-level-parity.gitlab-ci.yml` – Validates top-level `only` / `except` pipeline filtering and top-level cache inheritance.
- `only-except-sources.gitlab-ci.yml` – Validates legacy `only` / `except` pipeline-source selectors such as `schedules`, `merge_requests`, `pushes`, `api`, `triggers`, and `pipelines`.
- `filters.gitlab-ci.yml` – Tests `workflow`, `only`, `except`, tag-only jobs, and `rules:changes`/`rules:if` combos.
- `environments.gitlab-ci.yml` – Covers `environment` metadata, `on_stop`, manual stop jobs, `auto_stop_in`, and `action` values `stop`, `prepare`, `verify`, and `access`.
- `tag-ambiguity.gitlab-ci.yml` – Ensures Opal fails fast when multiple git tags point to `HEAD` and no explicit `CI_COMMIT_TAG`/`GIT_COMMIT_TAG` is provided.

## Running locally

Set the CI variables you care about before calling `opal run`. Use `env` or prefix assignments (or run them all via `./scripts/test-pipelines.sh`, which iterates through representative scenarios automatically):

```bash
# Exercise needs/dependencies on a branch pipeline
CI_COMMIT_BRANCH=main \
CI_PIPELINE_SOURCE=push \
opal run --pipeline pipelines/tests/needs-and-artifacts.gitlab-ci.yml

# Simulate a scheduled run that enables delayed jobs
CI_COMMIT_BRANCH=main \
CI_PIPELINE_SOURCE=schedule \
RUN_DELAYED=1 \
opal run --pipeline pipelines/tests/rules-playground.gitlab-ci.yml
```

Toggles to try:

- `CI_COMMIT_TAG=v1.2.3` – Enables the release-only jobs and manual approvals.
- `ENABLE_OPTIONAL=1` – Forces the optional build path in `needs-and-artifacts`.
- `FORCE_DOCS=1` – Triggers the manual `docs-or-flag` rule.
- `RUN_DELAYED=1` – Enables the delayed verifier in `rules-playground`.
- `SKIP_INHERIT=1` – Lets you confirm `inherit:default` handling in `includes-and-extends`.
- Touch files under `docs/` before running to satisfy the `changes:` rules.
- `FORCE_DOCS=1` – Triggers manual rules in `filters.gitlab-ci.yml`.
- Set `CI_COMMIT_BRANCH=main` or `CI_COMMIT_TAG=...` to watch `only`/`workflow` interactions in the filters fixture.
- For `environments.gitlab-ci.yml`, observe how `on_stop` and `action: stop` jobs are surfaced.
- Run the resource fixtures with `OPAL_TEST_COMMAND=run OPAL_TEST_ARGS="--no-tui --engine docker"` if you want to exercise actual containers; otherwise `OPAL_TEST_COMMAND=plan` (default) ensures the parser and scheduler agree with GitLab.

Use these samples when adding new features or debugging differences with GitLab—they give us fast, reproducible coverage without wiring up real projects.
