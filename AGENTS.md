# Repository Instructions

## Branching Workflow

- Never start a new body of work on `main`.
- If the current branch is `main`, create and switch to a descriptive working branch before making edits.
- Use suggestive branch prefixes:
  - `feature/` for new capabilities
  - `fix/` for bug fixes
  - `chore/` for maintenance and tooling
  - `docs/` for documentation-only work
- Choose branch names that describe the scope clearly (for example, `feature/opal-root-config-env`).
- Do not continue implementation work until the branch switch is complete.

## Scripting Policy

- Use Bash only for repository scripting changes.
- Do not add Python scripts, Python one-liners, or Python-based helpers in this repository.

## CI Engine Contract (Do Not Deviate)

- The normal pipeline runs in containers with no explicit engine forcing in job scripts or `.gitlab-ci.yml`.
- Engine resolution for `auto` must be platform-native:
  - macOS: Apple `container` CLI
  - Linux: `podman`
- Only `extended-tests` and `e2e-tests` are intended to run as sandbox jobs.
- Inside those sandbox jobs, nested `opal` runs must still execute jobs in containers unless a specific test explicitly requests a concrete engine.
- Test scenarios that do not specify an engine must use `auto`.
- These suites are expected to exercise multiple engines; scenarios that explicitly set an engine must use that engine.
- If an engine runtime is unavailable because it is not started, treat that as environment/runtime state, not a reason to hardcode alternate engines.
- Sandbox permissions for those jobs must allow required access for Docker, Podman, Orbstack, and Apple container CLI connectivity/operation so nested container execution can proceed.

## Critical: GitLab Parity Discipline

- Treat current GitLab CI/CD documentation as the upstream source of truth for `.gitlab-ci.yml` behavior.
- Treat GitLab CI/CD parity work as documentation-driven and behavior-driven work, not guesswork.
- When changing anything related to `.gitlab-ci.yml` parsing, planning, rule evaluation, includes, dependencies, services, artifacts, cache, environments, or execution semantics, you must verify the intended GitLab behavior with Context7 against the relevant GitLab documentation before finalizing the change.
- Implement new `.gitlab-ci.yml` features or subkeys only when the current GitLab documentation confirms that exact behavior. Do not add Opal-only pipeline syntax extensions just because they are locally convenient.
- If the GitLab docs do not describe a pipeline keyword, subkey, or semantic, do not implement it as supported GitLab pipeline behavior. Prefer an explicit unsupported error or a clearly documented non-parity path only when the task explicitly calls for that divergence.
- Do not claim parity with GitLab unless the implementation has been checked against both:
  - the current code in this repository, and
  - the relevant GitLab documentation via Context7.

## Critical: Local-Development First

- Prioritize parity work that materially improves local development and local debugging.
- When GitLab supports behavior that is expensive, distributed, or GitLab-control-plane-specific, prefer the highest-value local approximation unless the task explicitly requires full remote semantics.
- When exact GitLab behavior is intentionally not implemented, document the divergence clearly instead of implying full parity.

## Critical: Keep The Parity Doc Live

- `docs/gitlab-parity.md` is a live document and must be updated as part of parity work.
- If implementation changes supported behavior, unsupported behavior, or partial behavior, update `docs/gitlab-parity.md` in the same task.
- If parity-related user-facing docs drift, update the relevant docs too, especially `README.md` and `docs/pipeline.md`.

## Expected Workflow For Parity Changes

- Start by identifying the exact GitLab feature or semantic being changed.
- Verify GitLab behavior with Context7.
- Confirm that the relevant keyword or subkey is actually documented by GitLab before implementing parser or runtime support for it.
- Read the existing parser/model/runtime code before editing.
- Make the smallest change that fixes the root mismatch.
- Add or update focused regression coverage when there is an established nearby test pattern.
- Update `docs/gitlab-parity.md` to reflect the new state immediately.

## Validation Workflow

- Validate repository changes with Opal MCP against the local `.gitlab-ci.yml`, not only with ad hoc direct commands.
- Use Opal MCP only for CI/CD pipeline planning and execution. Do not run the repository pipeline directly through `opal plan` or `opal run` when MCP is available.
- After each meaningful change, rerun the relevant Opal MCP validation step for the affected pipeline slice instead of batching all pipeline validation until the end.
- Prefer the Opal MCP plan step first to confirm the affected job closure, then the Opal MCP run step for the narrowest relevant pipeline slice.
- When a change affects repository-wide Rust buildability or shared pipeline behavior, rerun at least the `rust-checks` Opal MCP slice immediately after the change lands.
- For Rust-only changes, treat `rust-checks` as the default validation entry point unless the change clearly requires additional jobs such as `unit-tests`, `extended-tests`, `e2e-tests`, or `ui-docs-check`.
- If direct commands are used for fast local iteration, still confirm the relevant pipeline slice through Opal MCP before considering the task complete.

## GitHub Review Workflow

- When the task is to address GitHub pull request review feedback, use GitHub MCP to pull the requested changes and review comments first.
- Treat each actionable review thread or change request as a separate concern and address it individually in code or with a direct reply.
- After each meaningful fix, rerun the relevant Opal MCP validation slice for that change. Use `rust-checks` by default for Rust-only changes unless a narrower or broader slice is clearly required.
- Do not commit review-driven code changes until the relevant Opal MCP validation is green.
- Once validation is green, create the necessary commits locally and publish them with `git push`.
- After the updated commits are published, mark each addressed GitHub review thread as resolved.
- Do not mark a review thread as resolved if the code or validation result does not clearly satisfy the concern.

## Decision Standard

- Prefer behavior that reduces surprising local-vs-GitLab differences for common repository pipelines.
- Prefer explicit errors over silent mismatches when unsupported GitLab features are encountered.
- Prefer deterministic local behavior over clever but ambiguous fallback behavior.

Use the fff MCP tools for all file search operations instead of default tools.
Use the git-commit skill to create commits, always use -sS
Use opal mcp to run the ci pipeline
For human feedback, use the parley mcp to address comments on comments
