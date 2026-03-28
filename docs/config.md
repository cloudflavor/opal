# Opal Configuration

Opal reads a layered `config.toml` to customize runtime behavior and automatically handle container registry authentication. Three locations are checked (earlier entries override later ones):

1. `$REPO/.opal/config.toml` – project-specific settings committed alongside your pipeline.
2. `$OPAL_HOME/config.toml` – machine-local runtime defaults for the selected Opal home.
3. `$XDG_CONFIG_HOME/opal/config.toml` (or the platform-default XDG config directory) – user-wide defaults.

This means project-level `.opal/config.toml` overrides global defaults.

## Example

```toml
[engine]
default = "docker"   # override --engine auto for this project or machine
preserve_runtime_objects = true

[ai]
default_provider = "ollama"
tail_lines = 200
save_analysis = true

[ai.prompts]
system_file = "prompts/ai/system.md"
job_analysis_file = "prompts/ai/job-analysis.md"

[ai.ollama]
host = "http://127.0.0.1:11434"
model = "qwen3-coder:30b"
system = "optional system prompt"

[container]         # applies to the Apple "container" CLI microVMs
arch = "arm64"       # optional; defaults to x86_64 unless overridden
cpus = "6"          # defaults to 4 if omitted
memory = "2g"       # defaults to 1.6 GB (1638m) if omitted
dns = "8.8.8.8"     # optional; leave unset to use the engine default

[[jobs]]
name = "deploy"
arch = "arm64"
privileged = true
cap_add = ["NET_ADMIN"]
cap_drop = ["MKNOD"]

[[registry]]
server = "registry.gitlab.com"
username = "gitlab-ci-token"
password_env = "CI_REGISTRY_PASSWORD"  # or `password = "plain-text"`
engines = ["container", "docker"]      # optional filter; empty list applies to every engine
scheme = "https"                       # optional for Apple `container` CLI
```

## Engine settings

You can set a config-level default engine for `--engine auto` with:

- `[engine].default`

Accepted values:

- `container`
- `docker`
- `podman`
- `nerdctl`
- `orbstack`

Additional engine-level controls:

- `preserve_runtime_objects`
  - default: `false`
  - when `true`, Opal keeps job/service runtime objects for inspection instead of cleaning them up automatically after successful job completion

CLI behavior still wins over config:

- explicit `--engine docker` beats config
- config default is used only when the CLI choice is `auto`

Runtime object preservation behavior:

- default behavior is to clean up job containers and service networks after jobs finish
- when `preserve_runtime_objects = true`, Opal keeps those runtime objects so you can inspect them manually after the run
- this is intended for debugging local container/service behavior, not for normal day-to-day cleanup

Currently only the Apple `container` CLI exposes tunables. You can configure it either via the dedicated `[container]` table (shown above) or the legacy `[engine.container]` table—both are merged, with `[container]` taking precedence.

- `arch`: string passed to `container run --arch`.
- `cpus`: string passed to `--cpus`. Controls maximum parallel threads in the VM.
- `memory`: string passed to `--memory`. Accepts Docker-style units (e.g., `1024m`, `2g`).
- `dns`: optional custom resolver for `container run --dns`.

Job-specific runtime overrides:

- Use `[[jobs]]` entries to target exact job names.
- Supported keys today:
  - `name`: exact job name to match
  - `arch`: override job architecture/platform selection
  - `privileged`: request privileged containers on engines that support it
  - `cap_add`: add Linux capabilities on engines that support it
  - `cap_drop`: drop Linux capabilities on engines that support it
- Engine behavior:
  - `docker`, `podman`, `nerdctl`, `orbstack`: support `privileged`, `cap_add`, and `cap_drop`
  - Apple `container`: supports `arch`, but fails explicitly if `privileged` or capability flags are requested
- Planning/execution interaction:
  - `opal plan --job <name>` and `opal run --job <name>` filter the execution plan first.
  - Any matching `[[jobs]]` override still applies to the selected job instances.

Add more `[engine.<name>]` tables in the future to tune other runtimes.

## AI settings

AI troubleshooting configuration is documented separately in `docs/ai-config.md`.

Use that page for:

- backend selection (`ollama`, `codex`, planned `claude`)
- prompt-template file overrides
- Ollama host/model settings
- Codex command/model settings
- analysis storage behavior

The current prompt-template placeholders are documented in `docs/ai.md`.

## Registry authentication

Each `[[registry]]` entry describes how to log into a container registry **before** jobs start:

- `server`: registry host (e.g., `registry.gitlab.com`).
- `username`: login name or token.
- `password` / `password_env`: either supply a literal password or point to an environment variable that contains it. One must be present.
- `engines`: optional list restricting the entry to specific engines (`container`, `docker`, `podman`, `nerdctl`, `orbstack`). Leave empty to apply everywhere.
- `scheme`: optional for Apple’s `container registry login`.

Opal pipes the resolved credentials into the correct CLI (`container registry login`, `docker login`, etc.), so you no longer have to run those manually on the host.

> Store secrets outside of version control. When committing a project-level `config.toml`, prefer `password_env` so tokens come from CI variables or local shell env vars.
