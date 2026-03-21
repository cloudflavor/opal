# GitLab Feature Parity

This page tracks which `.gitlab-ci.yml` features Opal currently recognizes and how that compares with GitLab's official CI/CD YAML surface.

Short answer: Opal is not on par with official GitLab today. It supports a useful local-runner subset, but GitLab's full YAML language and pipeline model are broader.

Last updated: 2026-03-21

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
- top-level `variables`
- top-level `image`
- top-level `workflow:rules`
- top-level `only` / `except`

### Reuse and composition

- hidden/template jobs (`.job-name`)
- `extends`
- `!reference`
- `inherit:default`
  - currently only used for `before_script` and `after_script`
- `include`
  - supported forms:
    - string path
    - `local:`
    - `file:`
    - `files:`
  - all supported include forms are resolved from the local filesystem

### Job execution and filtering

- `script`
- `before_script`
- `after_script`
- `when`
  - `manual`
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
- `variables`
- `timeout`
- `retry`
  - `max`
  - `when`
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
  - matrix-targeted needs
  - inline matrix variant references such as `build: [linux, release]`
- `dependencies`
- `parallel`
  - numeric fan-out
  - `parallel:matrix`

### Job data and runtime metadata

- `artifacts`
  - `paths`
  - `when`
  - `exclude`
  - `untracked`
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
  - `entrypoint`
  - `command`
  - `variables`
- `environment`
  - `name`
  - `url`
  - `on_stop`
  - `action`
  - `auto_stop_in`

## Partial Or Divergent Support

These features exist in Opal, but they do not match GitLab completely.

- `include` is local-only in practice.
  GitLab supports many include sources; Opal only resolves local paths and local file lists.
- `default` is subset-only.
  Unknown default keys are ignored.
- `inherit:default` is subset-only.
  Opal only models inheritance for `before_script` and `after_script`.
- `only` / `except` are narrower than GitLab.
  Opal matches branch names, tag names, exact refs, and regexes. GitLab supports a broader filter language.
- `artifacts` is subset-only.
  Opal models `paths`, `when`, `exclude`, and `untracked`, but not the broader artifacts feature set from GitLab.
- `cache` is subset-only.
  Opal models string keys and `key:files` + optional `prefix`, plus `paths`, `policy`, and `fallback_keys`, but not the full GitLab cache surface.
  Current behavior follows GitLab's practical shape for local runs:
  - up to two `key:files` entries
  - wildcard file patterns
  - non-existent files are ignored
  - if no files are present, the key falls back to `default` (or `<prefix>-default` when prefix is set)
- `services` are approximated through local container engines rather than matching GitLab Runner exactly.
  GitLab documents services as sidecar containers attached by the runner to a job network, with alias-based access and service-only variables. Opal mirrors the common local shape by starting sibling containers on a local engine network, normalizing aliases, honoring `entrypoint`, `command`, and `variables`, and injecting link-style connection env for some engines. It does not emulate the full range of runner-specific networking modes, service isolation rules, or executor-specific behavior from GitLab Runner.
- `retry.when` is parsed, but execution behavior is not modeled with GitLab's full retry policy semantics.
- `tags` are informational only.
  GitLab uses runner tags for scheduling; Opal logs and ignores them.
- `workflow` support is limited to `workflow:rules`.
  The broader workflow surface from GitLab is not implemented.

## Best Fit For Local Development

GitLab's YAML surface is much broader than what is worth mirroring locally. The best local-development targets are the features that change which jobs run, what data they see, and whether those jobs are fast and trustworthy on one machine.

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
- `environment`, `timeout`, `retry`, `interruptible`, and `resource_group`
  These affect local control flow and developer-visible behavior even without GitLab's remote orchestration layer.

Lower-value or intentionally out-of-scope local targets:

- remote/project/template/component `include`
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
  - project includes
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

### Priority 1: CI composition parity (high impact)

- Add non-local include sources where possible:
  - project includes
  - remote includes
  - template includes
- Keep local-first behavior deterministic:
  - cache fetched includes locally
  - surface explicit errors when remote includes cannot be resolved

### Priority 2: Control-flow parity (high impact)

- Expand pipeline orchestration semantics:
  - `trigger`
  - child pipelines
  - multi-project downstream pipelines
- Improve retry behavior to more closely follow GitLab semantics for `retry.when` at execution time.

### Priority 3: Job keyword surface (medium impact)

- Extend artifact feature coverage beyond `paths/when/exclude/untracked`.
- Extend cache feature coverage beyond current key/path/policy/fallback subset.
- Broaden `only`/`except` support where still narrower than GitLab.

### Priority 4: Runner-environment fidelity (medium impact)

- Narrow execution differences between local engines and GitLab Runner service networking/isolation.
- Continue improving log fidelity so failure context matches GitLab UI expectations more closely.

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
