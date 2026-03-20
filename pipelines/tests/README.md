# Test Pipelines

This directory contains `.gitlab-ci.yml` snippets that exercise the trickier parts of GitLab’s pipeline syntax so we can regression-test Opal locally. Each file can be passed to `opal run --pipeline <file>` along with CI-style environment variables to mimic different runners, branches, or triggers.

## Available scenarios

- `needs-and-artifacts.gitlab-ci.yml` – Covers `workflow:rules`, `default.before_script/after_script`, `!reference`, artifact passing via `needs`, `needs:optional`, `dependencies`, artifact exclusions, untracked artifact capture, manual/tagged releases, delayed jobs, environments, and `parallel:matrix` builds.
- `rules-playground.gitlab-ci.yml` – Focuses on the `rules:` mini-language: `if`, `changes`, `exists`, `when: manual|delayed`, inline `allow_failure`, schedule-only behavior, and interaction with `workflow:rules`.
- `includes-and-extends.gitlab-ci.yml` – Exercises local `include:`, hidden/template jobs, `extends`, `inherit: { default: [...] }`, and shared variables (see `job_inherit_flags` in `src/gitlab/parser.rs`).
- `resources-and-services.gitlab-ci.yml` – Validates caches, retries, timeouts, `interruptible`, `resource_group` locking, and job-specific `services` the way `src/gitlab/graph.rs` models them.
- `cache-policies.gitlab-ci.yml` – Validates local cache restore/save semantics, especially `cache:policy: pull` behavior where jobs can write to restored cache contents without persisting those changes back to the shared cache.
- `cache-fallback.gitlab-ci.yml` – Validates `cache:fallback_keys` restore behavior by seeding a default-branch cache and then restoring it from a feature-branch run when the primary key is missing.
- `filters.gitlab-ci.yml` – Tests `workflow`, `only`, `except`, tag-only jobs, and `rules:changes`/`rules:if` combos.
- `environments.gitlab-ci.yml` – Covers `environment` metadata, `on_stop`, manual stop jobs, and `auto_stop_in`.

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
