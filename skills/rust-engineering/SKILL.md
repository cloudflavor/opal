---
name: rust-engineering
description: Rust implementation, review, and quality standards for application code. Use when working on Rust code, reviewing Rust changes, writing or updating tests, verifying crate, framework, library, or API usage against documentation, improving code structure, or enforcing Rust-specific engineering constraints and validation steps.
---

# Rust Engineering

Apply these rules whenever working on Rust code.

## Work In Small Units

- Break work into small, well-defined units.
- Keep a live working plan and update it as progress is made.
- Organize code into logical modules; avoid monolithic files.
- Reject placeholders, TODOs, stubs, empty implementations, and partial scaffolding.
- Reject hacks, lazy workarounds, and shortcut implementations.
- Evaluate whether the approach is sound before implementing it.

## Verify External Behavior

- Use Context7 whenever correctness depends on crate, framework, library, or API behavior.
- Match the implementation to the documented behavior for the specific version or feature in use.
- Stop and verify instead of guessing when external behavior is unclear.

## Manage Dependencies Conservatively

- Ask for permission before adding, removing, replacing, or materially changing dependencies.
- Prefer simpler designs before introducing new libraries or abstractions.

## Handle Errors Properly

- Do not use `unwrap`, `expect`, or `panic` unless there is no reasonable alternative.
- Use proper error propagation throughout.
- Use `anyhow` for application-level errors unless the project already follows a different convention.
- Add error context where it improves diagnosis.

## Keep The Code Idiomatic

- Do not hardcode values that should be configurable or derived.
- Do not put fucntional code in mod.rs, only add modules and module exports.
- Prefer references over unnecessary owned allocations where reasonable.
- Avoid unnecessary `.clone()` and copying.
- Do not add disproportionate complexity only to avoid a small clone.
- Prefer returning iterators over allocating collections when that improves the design.
- Prefer small, composable functions over large functions.
- Prefer clear structure over ad hoc logic.
- Use established design patterns when they fit naturally.

## Test Every Change

- Always add or update tests for new behavior.
- Prefer end-to-end tests when practical.
- Introduce simple abstractions when online services would otherwise block offline tests.
- Do not treat the task as complete while the change remains untested without a clear reason.

## Validate Before Finishing

After meaningful Rust code changes, run:

- `cargo fmt`
- `cargo clippy`
- `cargo check`

Before considering the task complete:

- Run the relevant test suite.
- Confirm formatting, linting, compilation, and tests pass.
- Call out any remaining risk, limitation, or follow-up explicitly.

## Review For Long-Term Quality

Do not only verify that the code works. Also verify that it should be built this way.

Check for:

- Architectural fit
- Unnecessary complexity
- Hidden coupling
- Brittle abstractions
- Backward compatibility risks
- Interface stability
- Maintainability over time
- Operational implications

Prefer simple, clear, maintainable solutions over clever or over-engineered ones. Raise concerns when an approach is fragile, unsafe, conceptually weak, or likely to create maintenance problems.
