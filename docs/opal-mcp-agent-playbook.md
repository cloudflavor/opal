# Opal MCP Agent Playbook

This document is meant to be handed to a coding agent such as Codex.

It explains how to use the Opal MCP server effectively when working on a repository that uses Opal to run GitLab-style CI/CD pipelines locally.

## Purpose

Opal is built to help software engineers iterate faster on CI/CD by running pipeline jobs locally.

With MCP, an AI agent can use Opal as a local CI observability and execution layer:

- inspect pipeline structure before running anything
- run only the jobs that matter
- inspect logs and runtime summaries from recorded runs
- patch code or config based on actual local failures
- rerun narrowly, then validate broadly

The goal is to shorten the loop between:

1. make a change
2. evaluate the plan
3. run affected jobs locally
4. inspect failures
5. fix the root cause
6. rerun and confirm

## Opal MCP vs Context7 MCP

These two MCP servers solve different problems and complement each other.

### Context7

Use Context7 when the question is:

- what does a library or framework do?
- what is the documented behavior for a versioned API?
- what is the correct configuration syntax or documented contract?

Context7 is a documentation and code-example source.
It provides external truth.

### Opal

Use Opal when the question is:

- what does this repository's pipeline evaluate to right now?
- which jobs ran locally?
- which jobs failed, passed, or were skipped?
- what do the logs and runtime details say?

Opal is an execution and history source.
It provides local operational truth.

### Practical rule

- use Context7 to understand intended behavior
- use Opal to understand actual local pipeline behavior
- use both when changing CI/CD semantics or debugging a real pipeline failure

## What Opal MCP Exposes

At the MCP protocol level, Opal supports:

- server initialization
- tools
- resources

In the current Opal implementation, the most important parts are:

### Tools

- `opal_plan`
  - evaluates a local `.gitlab-ci.yml`
  - returns either a formatted plan or JSON plan
- `opal_run`
  - runs the local pipeline without the TUI
  - returns a recorded run summary
- `opal_view`
  - inspects the latest or a selected recorded run
  - can include job logs and runtime summaries
- `opal_history_list`
  - returns recorded runs with optional run-status and job-name filters
  - lets an agent narrow history before choosing a run to inspect
- `opal_failed_jobs`
  - returns the failed jobs for the latest or a selected recorded run
  - lets an agent jump directly to actionable failures before inspecting full run details
- `opal_plan_explain`
  - explains why a job is included, skipped, or blocked in the evaluated plan
  - helps an agent answer selector and rule questions without inferring from raw plan output

### Resources

Opal also exposes history and run resources that are ideal for browsing prior state:

- `opal://history`
- `opal://runs/latest`
- `opal://runs/<run_id>`
- `opal://runs/<run_id>/jobs/<job>/log`
- `opal://runs/<run_id>/jobs/<job>/runtime-summary`

These resources are especially valuable for agents because they support discovery and inspection without guessing run IDs or job names.

## Why Resources Matter

For an AI agent, resources are often as important as tools.

Tools answer:

- what action can I take?

Resources answer:

- what state already exists?
- what historical runs are available?
- what logs can I inspect before I rerun anything?

Without resources, an agent tends to over-rely on "latest run" behavior.
With resources, an agent can:

- locate yesterday's last failed run
- inspect a specific historical job log
- compare runs mentally before deciding what to rerun
- avoid unnecessary full-pipeline reruns

## Recommended Agent Operating Model

Treat Opal as a local CI control plane.

The preferred order of operations is:

1. discover history and current state
2. inspect the evaluated pipeline plan
3. inspect existing failures before rerunning
4. rerun only the failing job set and required upstream closure
5. patch the root cause
6. rerun the narrow slice
7. rerun the broader pipeline only after focused validation is clean

This is the fastest path for local CI iteration.

## Ideal MCP Flow For Opal

An agent using Opal well should follow this sequence.

### 1. Initialize the MCP session

Start with `initialize` and inspect:

- `serverInfo`
- `instructions`
- `capabilities`

Important things to learn immediately:

- whether the server supports tools
- whether the server supports resources
- whether resource listing and reading are available
- server version, if behavior may have changed

### 2. List tools

Call `tools/list` and inspect the input schemas.

For Opal, the core tools are:

- `opal_plan`
- `opal_run`
- `opal_view`
- `opal_history_list`
- `opal_failed_jobs`
- `opal_plan_explain`

Pay attention to arguments such as:

- `workdir`
- `pipeline`
- `jobs`
- `engine`
- `json`
- `include_log`
- `include_runtime_summary`

### 3. List resources

Call `resources/list`.

For Opal, this is critical because it reveals:

- history resources
- latest run resource
- per-run resources
- per-job log resources
- per-job runtime-summary resources

### 4. Read resources before rerunning

Before using `opal_run`, prefer to inspect what already happened.

Useful first reads:

- `opal://history`
- `opal://runs/latest`

Then, if you find a relevant run:

- `opal://runs/<run_id>`
- `opal://runs/<run_id>/jobs/<job>/log`
- `opal://runs/<run_id>/jobs/<job>/runtime-summary`

### 5. Plan before running

Use `opal_plan` before `opal_run` unless the task is explicitly just “rerun the exact same thing.”

Prefer JSON plan output when possible because it is easier for agents to reason over deterministically.

The agent should use the plan to answer:

- which jobs are eligible to run?
- which jobs depend on which upstream jobs?
- which jobs are manual, skipped, or gated?
- what is the smallest runnable subset needed for the issue at hand?

### 6. Run narrowly first

Use `opal_run` with targeted jobs whenever possible.

Do not default to a full pipeline run if the task can be narrowed.

Prefer:

- run only the failing job
- or run a small set of affected jobs
- let Opal include required upstream dependencies automatically

This reduces runtime, noise, and unnecessary failure surface.

### 7. Inspect the result deeply

After a run, use `opal_view` or resources to inspect:

- overall run status
- per-job status
- failed job logs
- runtime summaries when available

The agent should distinguish carefully between:

- execution-environment failures
- formatting/lint failures
- test failures
- config or metadata failures
- downstream skips caused by fail-fast behavior

### 8. Patch the root cause

Fix the narrowest root cause that explains the observed failure.

Avoid broad refactors unless the failure requires them.

### 9. Rerun the narrow slice

After patching, rerun only the affected jobs first.

Only after the narrow slice succeeds should the agent run the larger pipeline.

## Narrow-First Discipline

This is the key operating principle for Opal-driven agents.

Prefer this order:

1. inspect history
2. inspect plan
3. rerun one job or a small closure
4. fix
5. rerun the same narrow scope
6. widen validation

Avoid this order:

1. full pipeline run
2. inspect too many failures at once
3. patch several things without isolating causes
4. rerun the full pipeline again

The second pattern is slower, noisier, and harder for an agent to reason about.

## Mac-Specific Engine Guidance

On macOS, let Opal choose its default engine unless there is a strong reason to override it.

For agent behavior, this means:

- prefer the default engine first
- do not force `docker` just because Docker is installed
- only override the engine when the current task explicitly requires it or when debugging engine-specific behavior

If a non-default engine fails, that does not necessarily mean the default engine path is broken.

## Example Workflow For This Repository

In this repository, a strong Opal-agent workflow would be:

1. inspect history or the latest run
2. identify the failed jobs
3. inspect those specific job logs
4. call `opal_plan` to understand dependency closure
5. rerun only the failing jobs first
6. patch the root causes
7. rerun those same jobs
8. run the broader pipeline after the focused failures are clean

### Concrete example from this repo

An example failure pattern in this repository looked like this:

- `fetch-sources` succeeded
- `opal-install-smoke` succeeded
- `rust-checks` failed
- `ui-docs-check` failed
- downstream jobs were skipped due to fail-fast behavior

The right agent response is not to start with a full rerun.
The right response is:

1. inspect `rust-checks` log
2. inspect `ui-docs-check` log
3. understand whether those failures are independent
4. fix each root cause
5. rerun those targeted jobs

### Example diagnosis pattern

For a `rust-checks` failure, a good agent should determine whether the failure is due to:

- compilation
- formatting
- linting
- missing toolchain components

For a `ui-docs-check` failure, a good agent should determine whether the failure is due to:

- dependency installation
- a docs sync script
- incorrect manifest lookup
- generated docs drift
- framework build metadata

The agent should name the exact failing command and exact failing file whenever possible.

## What A Good Generic MCP Client Should Always Inspect

For any MCP server, not just Opal, a good client or agent should check:

### Initialization data

- protocol version
- server version
- capabilities
- server instructions

### Tool metadata

- tool names
- descriptions
- input schemas
- whether tools are stateful or read-only

### Resource metadata

- whether resources exist
- whether resources can be listed
- whether resources can be read directly
- whether resources provide historical or diagnostic value

### Prompt metadata

If the server supports prompts, inspect them too.

Prompts are useful for:

- canned workflows
- repeated task templates
- guided diagnosis flows

In Opal's current implementation, the important surfaces are tools and resources.

## Best Practices For Codex-Like Agents Using Opal

When operating as a coding agent:

- do not guess about pipeline state when Opal can tell you
- inspect logs before patching code
- prefer a plan before a run
- prefer a narrow run before a full run
- use run history to avoid repeating work
- separate root-cause failures from downstream skipped jobs
- use runtime summaries when container or service behavior matters
- use Context7 alongside Opal when version-specific external behavior is relevant

## Suggested Default Procedure For An Agent

Use this as the default playbook.

### Step 1: discover current state

- initialize MCP session
- list tools
- list resources
- inspect history or latest run

### Step 2: inspect plan

- call `opal_plan`
- prefer JSON output if the client can use it well
- identify the minimal affected job set

### Step 3: inspect failures

- read failed job logs
- read runtime summaries if available
- identify exact commands, files, and error messages

### Step 4: patch minimally

- fix the root cause
- avoid unrelated changes

### Step 5: rerun narrowly

- rerun only the affected job set
- verify the specific failure is gone

### Step 6: widen validation

- run additional dependent jobs or the full pipeline once the narrow scope is clean

## What Would Make Opal Even Better For Agents

If designing an agent-first Opal MCP experience, the following additions would be especially valuable:

- richer `opal_history_list`
  - add filters for date, branch, and pipeline file on top of the current status and job-name filters
- `opal_run_diff`
  - compare two runs and summarize what changed
- `opal_job_rerun`
  - rerun one job plus required upstream closure from an existing run
- `opal_engine_status`
  - report which local engine is healthy and usable
- `opal_logs_search`
  - search across historical logs for recurring failures

These are not required to use Opal effectively, but they would make agent workflows much more powerful.

## Short Agent Instruction Block

If you want a compact set of instructions to feed directly to a coding agent, use this:

> Use Opal MCP as the source of truth for local CI/CD behavior in this repository. Start by initializing the MCP session, listing tools, and listing resources. Prefer reading run history, latest run details, job logs, and runtime summaries before rerunning anything. Call `opal_plan` before `opal_run` to understand the evaluated DAG and identify the minimal affected job set. Prefer narrow runs over full pipeline runs, and let Opal include upstream dependency closure automatically. After a run, inspect failed job logs and runtime summaries, fix the root cause with minimal changes, rerun the same narrow job set, and only then widen validation to the broader pipeline. Use Context7 when correctness depends on documented external library or framework behavior.

## Bottom Line

For an AI agent, Opal MCP should be treated as a local CI observability-and-control layer.

The correct mental model is:

- discover history
- inspect plan
- inspect failures
- run narrowly
- patch the root cause
- rerun narrowly
- validate broadly

That is the fastest and most reliable workflow for software-engineering iteration with local CI/CD.
