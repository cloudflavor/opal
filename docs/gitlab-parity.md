# GitLab Feature Parity

This page tracks which `.gitlab-ci.yml` features Opal currently recognizes and how that compares with GitLab's official CI/CD YAML surface.

Short answer: Opal is not on par with official GitLab today. It supports a useful local-runner subset, but GitLab's full YAML language and pipeline model are broader.

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
- `cache`
  - `key`
  - `paths`
  - `policy`
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
  Opal models `paths`, but not the broader artifacts feature set from GitLab.
- `cache` is subset-only.
  Opal models `key`, `paths`, and `policy`.
- `services` are approximated through local container engines.
  Alias and startup behavior work for common cases, but this is not a full GitLab Runner service implementation.
- `retry.when` is parsed, but execution behavior is not modeled with GitLab's full retry policy semantics.
- `tags` are informational only.
  GitLab uses runner tags for scheduling; Opal logs and ignores them.
- `workflow` support is limited to `workflow:rules`.
  The broader workflow surface from GitLab is not implemented.

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
