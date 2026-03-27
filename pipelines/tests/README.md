# Test Pipelines

This directory contains `.gitlab-ci.yml` snippets that exercise the trickier parts of GitLab’s pipeline syntax so we can regression-test Opal locally. Each file can be passed to `opal run --pipeline <file>` along with CI-style environment variables to mimic different runners, branches, or triggers.

## Available scenarios

- `needs-and-artifacts.gitlab-ci.yml` – Covers `workflow:rules`, `default.before_script/after_script`, `!reference`, artifact passing via `needs`, `needs:optional`, `dependencies`, artifact exclusions, untracked artifact capture, manual/tagged releases, delayed jobs, environments, and `parallel:matrix` builds.
- `rules-playground.gitlab-ci.yml` – Focuses on the `rules:` mini-language: `if`, `changes`, `exists`, `when: manual|delayed`, inline `allow_failure`, schedule-only behavior, and interaction with `workflow:rules`.
- `includes-and-extends.gitlab-ci.yml` – Exercises local `include:`, hidden/template jobs, `extends`, `inherit: { default: [...] }`, and shared variables (see `job_inherit_flags` in `src/gitlab/parser.rs`).
- `yaml-merge-parity.gitlab-ci.yml` – Validates YAML merge-key (`<<`) support for merged job mappings and merged `variables` mappings.
- `inherit-default-parity.gitlab-ci.yml` – Validates full `inherit:default` behavior for the default keys Opal models today.
- `image-platform-parity.gitlab-ci.yml` – Validates GitLab-style `image:docker:platform` parsing and planner display.
- `image-platform-parity.gitlab-ci.yml` – Validates GitLab-style `image:entrypoint`, `image:docker:platform`, and `image:docker:user` in planner output and runtime execution.
- `includes-parity.gitlab-ci.yml` – Exercises local include parity behavior: repository-root `include:local`, wildcard local includes, `include:files`, and parse-time environment expansion in include paths.
- `include-surface.gitlab-ci.yml` – Exercises additional local include forms: bare string include entries and singular `include:file` entries.
- `include-remote-unsupported.gitlab-ci.yml` / `include-template-unsupported.gitlab-ci.yml` / `include-component-unsupported.gitlab-ci.yml` – Ensure unsupported non-local include types fail explicitly.
- `resources-and-services.gitlab-ci.yml` – Validates caches, retries, timeouts, `interruptible`, `resource_group` locking, and job-specific `services` the way `src/gitlab/graph.rs` models them.
- `resource-group-cross-run.gitlab-ci.yml` – Validates that `resource_group` locking applies across separate local Opal runs, not just within one process.
- `services-and-tags.gitlab-ci.yml` – Validates service string/mapping forms, multiple aliases, and informational runner tags in planner output.
- `services-docker-runtime.gitlab-ci.yml` – Validates runtime acceptance of `services:docker:platform` and `services:docker:user` on an engine that exposes Docker-style image options.
- `services-default-aliases.gitlab-ci.yml` – Validates GitLab-style default service aliases when no explicit alias is provided.
- `services-network-reachability.gitlab-ci.yml` – Validates that a job can actually reach a service by alias during runtime, which is especially important for the macOS `container` engine.
- `services-multi-alias-reachability.gitlab-ci.yml` – Validates that all accepted aliases for a service are reachable during runtime.
- `services-network-isolation.gitlab-ci.yml` – Validates that a service alias is reachable inside its own job but does not leak into a later job, proving per-job network isolation.
- `services-slow-start.gitlab-ci.yml` – Validates that Opal waits for a no-healthcheck service to become actually reachable before starting the job script.
- `services-variables.gitlab-ci.yml` – Validates that pipeline/job variables are passed to services while `services:variables` remain service-only and are not expanded against themselves.
- `services-invalid-alias.gitlab-ci.yml` – Ensures invalid service aliases fail explicitly instead of being silently normalized.
- `services-readiness-failure.gitlab-ci.yml` – Validates service readiness failure handling by starting a deliberately broken sidecar and expecting Opal to fail before job script execution.
- `cache-policies.gitlab-ci.yml` – Validates local cache restore/save semantics, especially `cache:policy: pull` behavior where jobs can write to restored cache contents without persisting those changes back to the shared cache.
- `cache-fallback.gitlab-ci.yml` – Validates `cache:fallback_keys` restore behavior by seeding a default-branch cache and then restoring it from a feature-branch run when the primary key is missing.
- `artifact-metadata.gitlab-ci.yml` – Validates `artifacts:name`, `artifacts:expire_in`, and dotenv report metadata in both planner output and downstream runtime behavior.
- `job-overrides-capabilities.gitlab-ci.yml` – Validates `[[jobs]]` runtime overrides for `cap_add` on engines that support Linux capability flags.
- `job-overrides-arch.gitlab-ci.yml` – Validates `[[jobs]]` runtime arch overrides for exact job-name matches.
- `retry-parity.gitlab-ci.yml` – Validates retry reruns for both `retry:when: script_failure` and `retry:exit_codes`, using the mounted Opal session directory so the first attempt fails and the retry succeeds.
- `dotenv-reports.gitlab-ci.yml` – Validates `artifacts:reports:dotenv` propagation through both `needs` and `dependencies`, and verifies that `needs:artifacts: false` plus `dependencies: []` block those variables.
- `control-flow-parity.gitlab-ci.yml` – Validates numeric `parallel`, top-level `image`/`variables`, rule-scoped `variables`, and `when: on_failure` behavior.
- `rules-compare-to.gitlab-ci.yml` – Validates `rules:changes:compare_to` against a temporary git worktree created by the harness.
- `needs-surface.gitlab-ci.yml` – Validates `needs:artifacts: false` and matrix-targeted `needs.parallel` planner behavior.
- `top-level-parity.gitlab-ci.yml` – Validates top-level `only` / `except` pipeline filtering and top-level cache inheritance.
- `only-except-sources.gitlab-ci.yml` – Validates legacy `only` / `except` pipeline-source selectors such as `schedules`, `merge_requests`, `pushes`, `api`, `triggers`, and `pipelines`.
- `only-except-variables.gitlab-ci.yml` – Validates deprecated `only:variables` and `except:variables` forms using the same expression language Opal already supports for `rules:if`.
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
