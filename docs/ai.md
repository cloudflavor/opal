# AI Troubleshooting

This page tracks Opal's AI-assisted job troubleshooting feature.

## Goal

The feature is meant to help developers understand why a selected local pipeline job failed.

The design target is:

- start from a selected job in the TUI
- build a bounded troubleshooting context from Opal's existing job, runner, YAML, and log data
- send that context to a configured AI backend
- stream the analysis back into the TUI
- optionally save the final analysis into the run session for later inspection

This is not meant to turn Opal into a general-purpose chat client.

It is a job-focused troubleshooting helper.

## Scope and current behavior

The first implementation is intentionally narrow:

- it works from a selected **current-run** job in the TUI
- it does not yet analyze arbitrary history entries directly
- it does not edit files or run fix-up commands
- it sends a bounded text context to the provider rather than asking the provider to explore the repository on its own

This keeps the first version deterministic and easier to trust.

## Current status

Implemented now:

- shared `src/ai/` module layout
- provider selection/config scaffolding
- `ollama` provider implementation
- embedded prompt templates under `prompts/ai/`
- config-based prompt file overrides
- TUI job action to request analysis
- streamed analysis rendering in the log pane
- optional saved analysis file under the run session

Planned next:

- `claude` provider via Claude Code CLI
- `codex` provider via Codex CLI
- provider picker / rerun with a different provider
- richer prompt/context extraction and saved analysis browsing in history mode

## Providers

### Ollama

Ollama is the first implemented backend.

Opal talks to the Ollama API directly and streams responses from the generate endpoint.

Configuration lives under:

```toml
[ai]
default_provider = "ollama"
tail_lines = 200
save_analysis = true

[ai.prompts]
system_file = ".opal/prompts/ai/system.md"
job_analysis_file = ".opal/prompts/ai/job-analysis.md"

[ai.ollama]
host = "http://127.0.0.1:11434"
model = "qwen3-coder:30b"
system = "optional system prompt"
```

`host` defaults to `http://127.0.0.1:11434`, but `model` is intentionally required in user config. Opal does not pick an Ollama model for you.

If the `ollama` provider is selected and `[ai.ollama].model` is missing or empty, Opal fails explicitly instead of choosing a model on your behalf.

## Prompt templates

Opal now ships embedded default prompt templates under:

```text
prompts/ai/system.md
prompts/ai/job-analysis.md
```

Users can override those at runtime with config file paths:

```toml
[ai.prompts]
system_file = ".opal/prompts/ai/system.md"
job_analysis_file = ".opal/prompts/ai/job-analysis.md"
```

Prompt templates use simple placeholder replacement rather than a full template engine.

Current placeholders:

- `{{job_name}}`
- `{{source_name}}`
- `{{stage}}`
- `{{runner_summary}}`
- `{{failure_hint}}`
- `{{job_yaml}}`
- `{{pipeline_summary}}`
- `{{runtime_summary}}`
- `{{log_excerpt}}`

Template precedence:

1. configured prompt file path
2. embedded default prompt

Paths in `[ai.prompts]` are resolved like other Opal config paths:

- absolute paths are used directly
- relative paths are resolved from the current project workdir

Prompt files are read at runtime, so users can iterate on prompts without rebuilding Opal.

### Claude Code

Planned.

The intended path is the Claude Code CLI in headless mode using structured output.

### Codex

Planned.

The intended path is `codex exec` in non-interactive mode using structured output.

## TUI usage

From the selected current-run job:

- press `a` to start analysis
- once analysis exists, press `a` again to toggle between the normal job logs and the AI analysis view
- while analysis is running, the selected job tab shows `ai…` and the `Details` pane shows `AI: running`
- after analysis completes, the job tab shows `ai` and the `Details` pane shows `AI: ready` or `AI: error`
- press `o` while the analysis view is active to open the current analysis text in your pager
- press `A` to preview the exact rendered AI prompt that Opal will send
- press `A` again while that prompt preview is open to close it immediately

The first version is current-run oriented. History-view AI actions can be added later.

## Context sent to the model

Opal builds a bounded troubleshooting prompt from:

- selected job name and stage
- selected job YAML
- runner info (engine, arch, CPU, RAM when known)
- concise dependency/needs summary
- runtime summary when available
- tail of the selected job log

Important: Opal sends the **contents** of those inputs, not just filesystem paths. This matters especially for Ollama, because the model cannot read local files unless their contents are explicitly included in the prompt.

The prompt is masked with Opal's existing secret masking before it is sent.

Current prompt construction steps are:

1. load the system template
2. load the job-analysis template
3. replace placeholders with the selected job context
4. mask secrets using Opal's existing masking rules
5. send the rendered text to the provider

## Storage

When `save_analysis = true`, Opal stores the final analysis under:

```text
$OPAL_HOME/<run-id>/<job-slug>/analysis/
```

For the Ollama backend, the first saved filename is:

```text
ollama.md
```

## Prompt preview

The prompt preview exists so you can inspect exactly what Opal is about to send before you rely on the model's diagnosis.

The preview shows:

- rendered system prompt
- rendered user/job-analysis prompt

This is useful when:

- you are editing custom prompt files
- you want to confirm a placeholder resolved correctly
- you want to verify that the right log excerpt and runner context are being sent

## What is not implemented yet

Still planned:

- Claude Code backend
- Codex backend
- provider picker / rerun with another backend
- saving and browsing prompt previews in history mode
- richer context extraction such as extracted high-signal error lines and upstream-failure summaries
