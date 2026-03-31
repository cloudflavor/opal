# Repository Instructions

## Critical: GitLab Parity Discipline

- Treat GitLab CI/CD parity work as documentation-driven and behavior-driven work, not guesswork.
- When changing anything related to `.gitlab-ci.yml` parsing, planning, rule evaluation, includes, dependencies, services, artifacts, cache, environments, or execution semantics, you must verify the intended GitLab behavior with Context7 against the relevant GitLab documentation before finalizing the change.
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
- Read the existing parser/model/runtime code before editing.
- Make the smallest change that fixes the root mismatch.
- Add or update focused regression coverage when there is an established nearby test pattern.
- Update `docs/gitlab-parity.md` to reflect the new state immediately.

## Validation Workflow

- Validate repository changes with Opal MCP against the local `.gitlab-ci.yml`, not only with ad hoc direct commands.
- Use Opal MCP only for CI/CD pipeline planning and execution. Do not run the repository pipeline directly through `opal plan` or `opal run` when MCP is available.
- Prefer the Opal MCP plan step first to confirm the affected job closure, then the Opal MCP run step for the narrowest relevant pipeline slice.
- For Rust-only changes, treat `rust-checks` as the default validation entry point unless the change clearly requires additional jobs such as `unit-tests`, `extended-tests`, `e2e-tests`, or `ui-docs-check`.
- If direct commands are used for fast local iteration, still confirm the relevant pipeline slice through Opal MCP before considering the task complete.

## Decision Standard

- Prefer behavior that reduces surprising local-vs-GitLab differences for common repository pipelines.
- Prefer explicit errors over silent mismatches when unsupported GitLab features are encountered.
- Prefer deterministic local behavior over clever but ambiguous fallback behavior.
