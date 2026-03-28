# AI Configuration

This page documents how to configure Opal's AI-assisted job troubleshooting backends, prompt files, and saved-analysis behavior.

## Example

```toml
[ai]
default_provider = "codex"
tail_lines = 200
save_analysis = true

[ai.prompts]
system_file = "prompts/ai/system.md"
job_analysis_file = "prompts/ai/job-analysis.md"

[ai.codex]
command = "codex"
model = "gpt-5-codex"

#[ai.ollama]
#host = "http://127.0.0.1:11434"
#model = "qwen3-coder:30b"
```

## Core settings

- `[ai].default_provider`
  - accepted values: `ollama`, `claude`, `codex`
  - implemented backends today: `ollama`, `codex`
  - `claude` is planned
  - when unset, Opal currently falls back to `ollama`
- `[ai].tail_lines`
  - number of trailing log lines to include in the troubleshooting context
- `[ai].save_analysis`
  - when `true`, Opal saves the final analysis into the run session

## Prompt files

Prompt overrides live under `[ai.prompts]`.

- `[ai.prompts].system_file`
  - optional path to a system-prompt template file
  - when set, overrides the embedded default system prompt
- `[ai.prompts].job_analysis_file`
  - optional path to a job-analysis prompt template file
  - when set, overrides the embedded default analysis prompt

Path resolution:

- absolute paths are used directly
- relative paths are resolved from the directory of the `config.toml` file that defined them

Examples:

- project config at `<repo>/.opal/config.toml`
  - `system_file = "prompts/ai/system.md"`
  - resolves to `<repo>/.opal/prompts/ai/system.md`
- user config at `$XDG_CONFIG_HOME/opal/config.toml`
  - `system_file = "prompts/ai/system.md"`
  - resolves to `$XDG_CONFIG_HOME/opal/prompts/ai/system.md`

Prompt files are read at runtime, so users can iterate on prompts without rebuilding Opal.

Embedded defaults live under:

```text
prompts/ai/system.md
prompts/ai/job-analysis.md
```

Supported placeholders:

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

## Ollama

Configuration keys:

- `[ai.ollama].host`
  - default: `http://127.0.0.1:11434`
- `[ai.ollama].model`
  - required when using the `ollama` provider
  - Opal does not choose a default Ollama model for you
- `[ai.ollama].system`
  - optional provider-level system prompt override

Example:

```toml
[ai]
default_provider = "ollama"

[ai.ollama]
host = "http://127.0.0.1:11434"
model = "qwen3-coder:30b"
```

## Codex

Configuration keys:

- `[ai.codex].command`
  - default: `codex`
  - command used to launch the Codex CLI backend
- `[ai.codex].model`
  - optional Codex model override
  - when unset, Codex CLI uses its own configured default model

Example:

```toml
[ai]
default_provider = "codex"

[ai.codex]
command = "codex"
model = "gpt-5-codex"
```

## Storage

When `save_analysis = true`, Opal stores saved output under:

```text
$OPAL_HOME/<run-id>/<job-slug>/analysis/
```

Backend-specific filenames currently include:

- `ollama.md`
- `codex.md`
