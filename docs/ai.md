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

## Quick start

For Codex CLI:

```toml
[ai]
default_provider = "codex"
tail_lines = 200
save_analysis = true

[ai.codex]
command = "codex"
model = "gpt-5-codex"
```

For Ollama:

```toml
[ai]
default_provider = "ollama"
tail_lines = 200
save_analysis = true

[ai.ollama]
host = "http://127.0.0.1:11434"
model = "qwen3-coder:30b"
```

For Claude Code:

```toml
[ai]
default_provider = "claude"
tail_lines = 200
save_analysis = true

[ai.claude]
command = "claude"
model = "sonnet"
```

Then in the TUI:

- select a job
- press `a` to analyze it
- press `a` again to switch between the normal log and the AI analysis
- press `A` to preview the exact rendered prompt
- press `o` to open the current log/analysis in your pager

## Scope and current behavior

The first implementation is intentionally narrow:

- it works from a selected **current-run** job in the TUI
- it also works for a selected job loaded from run history / `opal view`, using the stored log and runtime-summary data available for that history entry
- it does not yet analyze arbitrary history entries directly
- it does not edit files or run fix-up commands
- it sends a bounded text context to the provider rather than asking the provider to explore the repository on its own

This keeps the first version deterministic and easier to trust.

## Current status

Implemented now:

- shared `crates/opal/src/ai/` module layout
- provider selection/config scaffolding
- `ollama` provider implementation
- `claude` provider implementation
- `codex` provider implementation
- embedded prompt templates under `prompts/ai/`
- config-based prompt file overrides
- TUI job action to request analysis
- streamed analysis rendering in the log pane
- optional saved analysis file under the run session

Planned next:

- provider picker / rerun with a different provider
- richer prompt/context extraction and saved analysis browsing in history mode

## Providers

### Ollama

Ollama is the first implemented backend.

Opal talks to the Ollama API directly and streams responses from the generate endpoint.

Configuration lives under:

See `docs/ai-config.md` for the full configuration surface.

`host` defaults to `http://127.0.0.1:11434`, but `model` is intentionally required in user config. Opal does not pick an Ollama model for you.

If the `ollama` provider is selected and `[ai.ollama].model` is missing or empty, Opal fails explicitly instead of choosing a model on your behalf.

Operational notes:

- Ollama is called directly through its HTTP API
- Opal streams the response incrementally from the Ollama generate endpoint
- Ollama cannot read your local files by path, so Opal sends the selected job context as text content

## Prompt templates

Opal uses file-backed prompt templates with simple placeholder replacement.

See `docs/ai-config.md` for:

- embedded default prompt locations
- override file paths
- supported placeholders
- precedence rules

### Claude Code

Claude Code is implemented through the Claude Code CLI.

Opal launches `claude -p` in headless mode with `--output-format stream-json`, enables partial-message streaming, and appends the rendered system prompt with `--append-system-prompt` when one is configured.

The current backend is analysis-focused and starts Claude Code in `--permission-mode plan` so troubleshooting stays non-interactive and non-editing by default.

Configuration lives under:

```toml
[ai]
default_provider = "claude"

[ai.claude]
command = "claude"
model = "sonnet"
```

Current defaults:

- `command` defaults to `claude`
- `model` is optional; when unset, Claude Code uses its own configured default model

Operational notes:

- Claude Code must already be installed and authenticated on the host
- Opal launches Claude Code from the repository workdir so it can inspect the current project context
- Opal sends the rendered troubleshooting context on stdin
- Opal streams text deltas from Claude Code `stream-json` output when available
- if Claude Code emits no partial deltas, Opal still captures the final assistant/result text from the structured stream before showing or saving the analysis

### Codex

Codex is implemented through the Codex CLI.

Opal uses `codex exec` in non-interactive mode, streams assistant deltas from JSON output, and captures the final message with `--output-last-message`.

The current backend is analysis-focused and launches Codex in a read-only, non-approval flow by default.

Configuration lives under:

```toml
[ai]
default_provider = "codex"

[ai.codex]
command = "codex"
model = "gpt-5-codex"
```

Current defaults:

- `command` defaults to `codex`
- `model` is optional; when unset, Codex CLI uses its own configured default model

Operational notes:

- Codex must already be installed and authenticated on the host
- Opal runs Codex in non-interactive mode
- Opal sends the rendered troubleshooting context on stdin
- Opal streams assistant message deltas when available from Codex JSON output
- if Codex produces little or no streamed delta content, Opal still loads the final saved response back into the analysis pane when the command completes

## TUI usage

From the selected job in the TUI:

- press `a` to start analysis
- once analysis exists, press `a` again to toggle between the normal job logs and the AI analysis view
- while analysis is running, the selected job tab shows `ai…` and the `Details` pane shows `AI: running`
- after analysis completes, the job tab shows `ai` and the `Details` pane shows `AI: ready` or `AI: error`
- press `o` while the analysis view is active to open the current analysis text in your pager
- press `A` to preview the exact rendered AI prompt that Opal will send
- press `A` again while that prompt preview is open to close it immediately

In loaded history / `opal view` mode, Opal builds the troubleshooting context from the stored job log, current pipeline YAML lookup, and any recorded runtime summary path for that historical job.

Current visible UI signals:

- selected job tab shows `ai…` while analysis is running
- selected job tab shows `ai` once analysis exists
- `Details` shows the current backend and AI status, for example `AI: codex running`

## Context sent to the model

Opal builds a bounded troubleshooting prompt from:

- selected job name and stage
- selected job YAML
- runner info (engine, arch, CPU, RAM when known)
- concise dependency/needs summary
- runtime summary when available
- tail of the selected job log

Important: Opal sends the **contents** of those inputs, not just filesystem paths. This matters especially for Ollama, because the model cannot read local files unless their contents are explicitly included in the prompt.

The same design is used for Codex too. Even though Codex can operate in a repository-aware environment, Opal still sends a bounded rendered troubleshooting prompt instead of relying on the provider to discover context on its own.

The prompt is masked with Opal's existing secret masking before it is sent.

For Codex, Opal still sends the rendered prompt **content** rather than just handing Codex a path to a log file. This keeps the troubleshooting request deterministic and consistent with the Ollama path.

Current prompt construction steps are:

1. load the system template
2. load the job-analysis template
3. replace placeholders with the selected job context
4. mask secrets using Opal's existing masking rules
5. send the rendered text to the provider

Prompt preview exists specifically so you can verify this rendered context before it is sent.

## Storage

When `save_analysis = true`, Opal stores the final analysis under:

```text
$OPAL_HOME/<run-id>/<job-slug>/analysis/
```

For the Ollama backend, the first saved filename is:

```text
ollama.md
```

For the general run/session storage layout, see `docs/storage.md`.

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
- provider picker / rerun with another backend
- saving and browsing prompt previews in history mode
- richer context extraction such as extracted high-signal error lines and upstream-failure summaries

## Troubleshooting AI integrations

If Ollama analysis fails:

- confirm the server is reachable at `[ai.ollama].host`
- confirm the model named in `[ai.ollama].model` is installed locally

If Codex analysis fails:

- confirm `codex` is on `PATH`
- confirm your Codex CLI authentication is already configured
- confirm the configured `command` and optional `model` are valid for your local Codex installation

## Related docs

- `docs/ai-config.md`
- `docs/ui.md`
- `docs/storage.md`

If the analysis pane shows little or no streamed content:

- wait for the final completion event
- Opal will still load the final returned text into the analysis pane if the backend produced a saved final message
