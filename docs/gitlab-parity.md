# GitLab Feature Parity

This page tracks which `.gitlab-ci.yml` features Opal currently recognizes and how that compares with GitLab's official CI/CD YAML surface.

Short answer: Opal is not on par with official GitLab today. It supports a useful local-runner subset, but GitLab's full YAML language and pipeline model are broader.

Last updated: 2026-03-26

## Recognized By Opal

### Pipeline structure and defaults

- `stages`
- `default`
  - `image`
  - `before_script`
  - `after_script`
  - `variables`
  - `cache`
  - `services`
  - `timeout`
  - `retry`
  - `interruptible`
- top-level `cache`
- top-level `variables`
- top-level `image`
- top-level `workflow:rules`
- top-level `only` / `except`

### Reuse and composition

- hidden/template jobs (`.job-name`)
- YAML merge keys (`<<`)
- `extends`
- `!reference`
- `inherit:default`
  - `false`
  - list form for the default keys Opal models today:
    - `image`
    - `before_script`
    - `after_script`
    - `cache`
    - `services`
    - `timeout`
    - `retry`
    - `interruptible`
- `include`
  - supported forms:
    - string path
    - `local:`
    - `file:`
    - `files:`
    - `project:`
      - requires GitLab credentials/configuration
  - all supported include forms are resolved from the local filesystem
  - local include paths are resolved from the repository root
  - local include wildcard paths such as `configs/*.yml`
  - parse-time variable expansion in local include paths
  - included local files must be `.yml` or `.yaml`
  - nested direct `include:local` inside fetched `include:project` files follows the included project context
  - unsupported non-local include forms fail explicitly:
    - `remote:`
    - `template:`
    - `component:`

### Job execution and filtering

- `script`
- `before_script`
- `after_script`
- `when`
  - `manual`
  - `delayed`
  - `never`
  - `always`
  - `on_failure`
  - `on_success`
- `rules`
  - `if`
  - `changes`
  - `changes:compare_to`
  - `exists`
  - `when`
    - `on_success`
    - `manual`
    - `delayed`
    - `never`
    - `always`
    - `on_failure`
  - `allow_failure`
  - `start_in`
  - `variables`
- `only` / `except`
  - exact ref names
  - regex ref filters
  - `branches`
  - `tags`
- `image`
  - string form
  - mapping form with `name`
  - `image:entrypoint`
  - `image:docker:platform`
  - `image:docker:user`
- `services`
  - string form
  - mapping form with `name`
  - `alias`
  - `entrypoint`
  - `command`
  - `variables`
  - `services:docker:platform`
  - `services:docker:user`
- `variables`
- `timeout`
- `retry`
  - `max`
    - validated to GitLab's documented `0`, `1`, or `2` range
  - `when`
  - `exit_codes`
- `interruptible`
- `resource_group`
- `tags`
  - parsed, but ignored for execution because Opal always runs locally

### Graph and dependency features

- implicit stage ordering
- `needs`
  - local job needs
  - `artifacts: true|false`
  - `optional: true`
  - `needs:project` with `ref`
    - requires GitLab credentials at runtime for cross-project artifact download
  - matrix-targeted needs
  - inline matrix variant references such as `build: [linux, release]`
- `dependencies`
- `parallel`
  - numeric fan-out
  - `parallel:matrix`

### Job data and runtime metadata

- `artifacts`
  - `name`
  - `paths`
  - `when`
  - `expire_in`
  - `exclude`
  - `untracked`
  - `reports`
    - `dotenv`
- `cache`
  - `key` (string form)
  - `key:files`
  - `key:prefix` (with `key:files`)
  - `paths`
  - `policy`
  - `fallback_keys`
- `services`
  - string form
  - mapping form with `name` / `image`
  - `alias`
    - single alias
    - comma-separated multiple aliases
    - GitLab-style default aliases derived from the image name when no explicit alias is set
  - `entrypoint`
  - `command`
  - `variables`
    - service-only variables are passed only to the service container
    - service-only variables are not expanded against themselves
- `environment`
  - `name`
  - `url`
  - `on_stop`
  - `action`
    - `stop`
    - `prepare`
    - `verify`
    - `access`
  - `auto_stop_in`

## Partial Or Divergent Support

These features exist in Opal, but they do not match GitLab completely.

- `include` is local-only in practice.
  GitLab supports many include sources; Opal fully resolves local paths and now has partial `include:project` support.
  Opal accepts standalone plain `file:` / `files:` include entries as local filesystem conveniences when no non-local include type is present; this does not mirror GitLab's `include:project` semantics.
  Opal expands include paths using environment available at parse time, which is useful locally and broadly matches GitLab's "include is evaluated before jobs" model, but it does not fully reproduce GitLab's exact server-side variable-availability rules.
  `include:project` currently depends on explicit GitLab credentials/configuration, resolves files through the GitLab API into a local cache, and supports nested direct `include:local` resolution within the fetched project context.
  Wildcard local includes inside fetched `include:project` configs are not yet supported.
  Opal does not yet support other non-local include sources.
- `default` is subset-only.
  Unknown default keys are ignored.
- workspace/source preparation is intentionally local-first.
  GitLab Runner normally prepares job sources through Git operations (`clone` / `fetch` / checkout / clean) and can remove untracked and ignored files depending on runner settings such as `GIT_STRATEGY`, `GIT_CHECKOUT`, and `GIT_CLEAN_FLAGS`.
  Opal intentionally does not force that remote-runner source lifecycle for local development. Instead it snapshots the current working tree so dirty tracked edits and in-progress local source changes are available to jobs.
  Opal currently copies `.git` into that local snapshot so Git-aware local behavior still works inside jobs and during local ref/tag evaluation.
  Opal still applies Git-aware filtering to avoid copying obvious local junk into the job workspace, including ignored/generated directories such as `target/`, `tests-temp/`, `.opal/`, `node_modules/`, and `.svelte-kit/`.
  This is a deliberate local-development divergence rather than a claim of exact GitLab Runner parity.
- `inherit:default` is subset-only.
  Opal now models `inherit:default` for the default keys it supports today:
  - `image`
  - `before_script`
  - `after_script`
  - `cache`
  - `services`
  - `timeout`
  - `retry`
  - `interruptible`
  It does not yet model GitLab keyword inheritance outside that supported default-key subset.
- `only` / `except` are narrower than GitLab.
  Opal accepts only string/list filter values and matches only:
  - exact ref names
  - regex ref filters
  - `branches`
  - `tags`
  - `merge_requests`
  - `schedules`
  - `pushes`
  - `api`
  - `web`
  - `triggers`
  - `pipelines`
  - `external_pull_requests`
  - `variables`
  Unsupported `only` / `except` forms in Opal today include:
  - change-based selectors
  - Kubernetes-based selectors
  - any other GitLab selector outside the list above
- `artifacts` is subset-only.
  Opal models only:
  - `name`
  - `paths`
  - `when`
  - `expire_in`
  - `exclude`
  - `untracked`
  - `reports:dotenv`
  Unsupported artifact keys in Opal today are:
  - `expose_as`
  - `public`
  - `access`
  - artifact reports other than `reports:dotenv`
  Artifact path behavior that now matches common GitLab usage:
  - directory artifact paths
  - file artifact paths
  - wildcard/glob artifact paths in `artifacts:paths`
  - wildcard-matched artifact files are collected and passed downstream through `needs` / `dependencies`
- `cache` is subset-only.
  Opal models only:
  - `key`
  - `key:files`
  - `key:prefix`
  - `paths`
  - `policy`
  - `fallback_keys`
  Unsupported cache keys in Opal today are:
  - `unprotect`
  - any other cache subkey outside the list above
  Current behavior follows GitLab's practical shape for local runs:
  - up to two `key:files` entries
  - wildcard file patterns
  - non-existent files are ignored
  - if no files are present, the key falls back to `default` (or `<prefix>-default` when prefix is set)
- `services` are approximated through local container engines rather than matching GitLab Runner exactly.
  Opal supports only:
  - string form
  - mapping form with `name` / `image`
  - `alias`
    - including comma-separated multiple aliases
  - `entrypoint`
  - `command`
  - `variables`
  Opal now validates aliases explicitly and fails when aliases contain unsupported characters instead of silently rewriting them.
  Unsupported service syntax in Opal today is any service subkey outside the list above.
  GitLab documents services as sidecar containers attached by the runner to a job network, with alias-based access and service-only variables. Opal mirrors the common local shape by starting sibling containers on a local engine network, normalizing aliases, honoring `entrypoint`, `command`, and `variables`, and injecting link-style connection env for some engines. It does not emulate the full range of runner-specific networking modes, service isolation rules, or executor-specific behavior from GitLab Runner.
  Opal now also performs a readiness gate after service start by inspecting container state/health and waiting up to a bounded timeout before running the job script. For engines without healthchecks, Opal requires a brief stable-running confirmation before treating the service as ready. This still does not reproduce all GitLab Runner wait-probe semantics. If service inspection is unavailable, Opal logs a warning and continues without the readiness gate.
  For the macOS `container` engine, Opal also injects service alias host entries into the job container so service aliases remain reachable during runtime even though the engine does not expose GitLab Runner-style network aliasing directly.
  For services without healthchecks but with discoverable TCP ports, Opal now waits for actual port reachability instead of treating process liveness alone as readiness.
- `interruptible` is partially modeled.
  Opal now applies `interruptible` during local pipeline abort flows by cancelling running jobs marked `interruptible: true` while allowing running non-interruptible jobs to finish.
  This is a local approximation of GitLab's auto-cancel behavior, not a full implementation of GitLab's redundant-pipeline and `workflow:auto_cancel` semantics.
- `resource_group` is local-only.
  Opal now serializes matching jobs across separate local Opal runs on the same machine by using a filesystem-backed lock under `OPAL_HOME`.
  This is still a local approximation rather than GitLab's distributed coordination across runners and pipelines.
- `needs:project` is partial runtime support.
  Parsing and artifact mounting are implemented, but cross-project artifact download requires explicit GitLab credentials/configuration (`--gitlab-token`, optionally `--gitlab-base-url`) and network access to the GitLab API. Opal models artifact download only; it does not reproduce GitLab's server-side orchestration model.
- `include:project` is partial runtime support.
  Opal can resolve project includes when explicit GitLab credentials/configuration are provided, but this is currently a local fetch-and-cache approximation through the GitLab API rather than full GitLab server-side config resolution semantics.
  Nested direct local includes within the fetched project are supported, but wildcard nested local includes are not yet.
- `retry` is still subset-only.
  Opal now validates `retry:max` against GitLab's documented `0..=2` range, accepts GitLab's documented `retry:when` condition names, and supports `retry:exit_codes`.
  Opal classifies a broader local subset of retry failure classes at execution time, including:
  - `unknown_failure`
  - `script_failure`
  - `api_failure`
  - `stuck_or_timeout_failure`
  - `runner_system_failure`
  - `runner_unsupported`
  - `job_execution_timeout`
  - `unmet_prerequisites`
  - `scheduler_failure`
  - `data_integrity_failure`
  - `stale_schedule`
  - `archived_failure`
  GitLab-specific failure sources such as `stale_schedule` and `archived_failure` are rare in Opal's local execution model, but retry matching now recognizes them when those failure states are surfaced.
- `environment.action` is subset-only.
  Opal explicitly models:
  - `stop`
  - `prepare`
  - `verify`
  - `access`
  Unsupported environment behavior in Opal today includes:
  - `environment:kubernetes`
- `tags` are informational only.
  GitLab uses runner tags for scheduling; Opal logs and ignores them.
- `image` is subset-only.
  Opal supports string form, mapping form with `name`, `image:entrypoint`, `image:docker:platform`, and `image:docker:user`.
  On `docker`, `podman`, `nerdctl`, and `orbstack`, Opal forwards `image:docker:platform` to the engine's `--platform` selection.
  On the Apple `container` engine, `image:docker:platform` is translated into the corresponding `container run --arch` selection for common `amd64` / `arm64` Linux platform values.
  `image:entrypoint` and `image:docker:user` are forwarded to the local engine's entrypoint/user flags where supported.
  Unsupported image behavior in Opal today includes:
  - `image:kubernetes`
  - executor-specific image options outside `docker:platform` / `docker:user`
- `services` is subset-only.
  Opal supports string services plus mapping entries with `name`, `alias`, `entrypoint`, `command`, `variables`, `services:docker:platform`, and `services:docker:user`.
  On `docker`, `podman`, `nerdctl`, and `orbstack`, Opal forwards `services:docker:platform` and `services:docker:user` to the local engine's service container flags.
  On the Apple `container` engine, `services:docker:platform` is translated into `container run --arch`, `services:docker:user` is forwarded to `container run --user`, and Opal now fails fast when the engine's per-job network creation stalls instead of hanging indefinitely.
  Unsupported service behavior in Opal today includes:
  - `services:kubernetes`
  - executor-specific service options outside `docker:platform` / `docker:user`
- `workflow` support is limited to `workflow:rules`.
  The broader workflow surface from GitLab is not implemented.
- tag trigger source is now explicit-only.
  GitLab pipelines are created by a single explicit ref event. Opal no longer infers tag context from local tags on `HEAD` during ordinary local runs. Tag-pipeline behavior now requires an explicit `CI_COMMIT_TAG` or `GIT_COMMIT_TAG`, and ambiguous explicit tag resolution still fails fast instead of guessing.

## Best Fit For Local Development

GitLab's YAML surface is much broader than what is worth mirroring locally. The best local-development targets are the features that change which jobs run, what data they see, and whether those jobs are fast and trustworthy on one machine.

## Which Gaps Matter Locally

Some unsupported GitLab features are good local-dev candidates because they change what runs, what data jobs see, or whether a local failure is trustworthy. Others are mostly GitLab control-plane or UI behavior and have low value in a single-checkout local runner.

High-value local candidates:

- additional `artifacts:reports` coverage beyond `reports:dotenv`
- broader `only` / `except` selectors when real repository pipelines rely on them
- service lifecycle and readiness fidelity

Mostly GitLab control-plane or UI behavior:

- `artifacts:expose_as`
- `artifacts:public`
- `artifacts:access`
- most `artifacts:reports` behavior that exists to feed GitLab UI/reporting features
- `cache:unprotect`
- runner `tags` scheduling semantics
- `environment:kubernetes`

High-value local-first features:

- `workflow:rules`, job `rules`, and job/pipeline `only` / `except`
  These decide whether Opal runs the same jobs a developer would otherwise wait for in CI.
- local composition features
  `include:local`, hidden jobs, `extends`, `!reference`, and `inherit:default` matter because real repository pipelines are heavily templated.
- `needs`, `dependencies`, and `parallel:matrix`
  These define local execution order, fan-out, and artifact flow.
- `artifacts`
  Artifact passing is critical for chained local jobs such as build -> test -> package.
- `cache`
  Cache fidelity directly affects local feedback time for ecosystems such as Rust, Node, Python, and Java.
- `services`
  Local databases, message brokers, and Docker sidecars are common reasons to reproduce CI jobs before pushing.
- `environment`, `timeout`, `retry`, and `resource_group`
  These affect local control flow and developer-visible behavior even without GitLab's remote orchestration layer.

Lower-value or intentionally out-of-scope local targets:

- remote/template/component `include`
  These are useful in GitLab-managed estates, but they depend on remote config resolution rather than a single local checkout.
- `trigger`, child pipelines, and multi-project pipelines
  These are orchestration features for distributed CI, not core single-repo local execution features.
- Pages, release jobs, and GitLab-managed deployment features
  They matter in GitLab's control plane, but they are not usually what a developer wants from a fast local pre-push loop.
- identity, `id_tokens`, and GitLab secret-management features
  These depend on GitLab-issued credentials and hosted integrations.
- runner tags and protected-runner routing
  GitLab uses these for remote runner selection. Opal always runs on the local machine, so the scheduling meaning does not translate.

## Major GitLab Features Missing From Opal

GitLab's official CI/CD YAML surface is broader than the subset above. The main missing areas are:

- advanced `include` sources
  - full `include:project` parity
  - remote includes
  - template includes
  - component includes
- header/configuration features such as `spec`
- downstream pipeline features
  - `trigger`
  - child pipelines
  - multi-project pipelines
- release-oriented and GitLab-managed deployment features
- Pages/reporting-oriented job features
- identity, token, and secret-management keywords from GitLab's YAML surface
- the rest of GitLab's job keyword surface beyond the subset Opal parses today

## Prioritized Parity Roadmap

This is the practical order for closing the highest-value parity gaps for day-to-day repository pipelines.
It is ordered by what is most likely to unblock real repository configs and reduce surprising local-vs-GitLab behavior.

### Priority 1: Runtime control-flow fidelity (highest impact)

- Keep refining failure classification so GitLab retry conditions map to local runtime errors more precisely.
- Extend `interruptible` beyond the current abort-flow approximation toward fuller `workflow:auto_cancel` parity where practical.
- Narrow service behavior gaps that matter in local debugging:
  - readiness detection
  - engine-specific command handling
  - runner-like network and lifecycle behavior where feasible
- Tighten `needs:project` runtime behavior and error reporting so credential/network requirements are explicit and easier to diagnose.

### Priority 2: Local composition correctness (high impact)

- Finish the remaining high-value local composition gaps:
  - nested wildcard `include:local` inside fetched `include:project` configs
  - any remaining `extends` / `!reference` / include merge edge cases found in real repositories
- Keep non-local resolution deterministic for local use:
  - cache fetched includes locally
  - surface explicit errors when remote includes cannot be resolved
- Add more non-local include sources only where they materially unblock real repository configs:
  - template includes
  - remote includes

### Priority 3: Job keyword surface parity (medium impact)

- Extend artifact feature coverage beyond `paths/when/exclude/untracked`.
- Extend cache feature coverage beyond current key/path/policy/fallback subset.
- Broaden `only`/`except` support where still narrower than GitLab.
- Broaden `environment.action` handling beyond the current `stop` subset where practical for local metadata and UX.

### Priority 4: Runner-environment fidelity (medium impact)

- Continue improving log fidelity so failure context matches GitLab UI expectations more closely.

## Regression Harness State

The local parity harness currently has two layers:

- planner coverage via `opal plan`
  - exercises parsing, filters, workflow/rules evaluation, include forms, dependency graph construction, matrix expansion, top-level filters, services/tags metadata, and environment metadata without starting containers
- runtime coverage via `opal run --no-tui`
  - exercises artifacts/dependencies, `artifacts:reports:dotenv`, cache restore/save behavior, retry handling, `when: on_failure`, service startup/readiness, secret masking, and environment/manual-job behavior against a real local container engine

Current harness characteristics:

- `scripts/test-pipelines.sh` auto-detects a usable local engine for runtime scenarios and fails fast when no engine is available.
- the repository's own `.gitlab-ci.yml` now avoids trying to run Opal inside Opal when it is itself executed by an installed `opal run`, so direct local self-hosted pipeline runs can exercise the main package path on a clean checkout.
- local-only scenarios are broadly covered for the subset Opal claims to support for day-to-day repository pipelines.
- GitLab-credentialed remote-success paths such as successful `include:project` / `needs:project` still do not have local harness coverage.
- Full GitLab control-plane behaviors such as redundant-pipeline auto-cancel modes, distributed `resource_group`, and downstream pipeline orchestration remain outside the current local harness scope.

### Priority 5: Distributed CI orchestration (lower local value, large scope)

- Expand pipeline orchestration semantics:
  - `trigger`
  - child pipelines
  - multi-project downstream pipelines
- Treat these as later work unless a concrete local-debugging workflow requires them, because they add significant complexity and have lower single-checkout local-runner value than the priorities above.

## Practical Conclusion

Opal currently covers a solid local-debugging subset of GitLab CI:

- templates and reuse
- rules and workflow filtering
- needs, dependencies, and matrix expansion
- artifacts, cache, and services
- timeouts, retries, environments, and resource groups

That is enough for many repository-local pipelines, but it is not feature-complete relative to GitLab's official syntax reference. In particular, configuration composition across projects, downstream pipelines, and many GitLab-specific job keywords are still outside Opal's model.

## Official References

- [GitLab CI/CD YAML syntax reference](https://docs.gitlab.com/ci/yaml/)
- [GitLab workflow reference](https://docs.gitlab.com/ci/yaml/workflow/)
- [GitLab job rules reference](https://docs.gitlab.com/ci/jobs/job_rules/)
- [GitLab downstream pipelines](https://docs.gitlab.com/ci/pipelines/downstream_pipelines/)
- [GitLab CI/CD components](https://docs.gitlab.com/ci/components/)
