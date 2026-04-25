# Opal Configuration

Opal reads a layered `config.toml` to customize runtime behavior and automatically handle container registry authentication. Two locations are checked (later entries override earlier ones):

1. `$XDG_CONFIG_HOME/opal/config.toml` (or the platform-default XDG config directory) – user-wide defaults.
2. `$REPO/.opal/config.toml` – project-specific settings committed alongside your pipeline.

This means project-level `.opal/config.toml` overrides global/user config.

## Example

```toml
[engine]
default = "docker"   # override --engine auto for this project or machine
preserve_runtime_objects = true
map_host_user = true  # map image/services user to host uid:gid when docker:user is unset

[env]
RUNNER_BOOTSTRAP = "enabled"
RUNNER_INIT_SCRIPT = "/opal/bootstrap/init.sh"

[bootstrap]
command = "bash .opal/bootstrap/prepare-runner.sh"
env_file = "bootstrap/generated.env"

[bootstrap.env]
RUNNER_HELPER = "/opal/bootstrap/scripts/helper.sh"

[[bootstrap.mounts]]
host = "bootstrap/scripts"
container = "/opal/bootstrap/scripts"
read_only = true

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

[sandbox]
debug = true
allowed_domains = ["github.com", "*.github.com", "api.github.com"]
allow_write = [".", "/tmp"]
ignore_violations = { "*" = [".cargo/registry/src"] }
enable_weaker_nested_sandbox = false
mandatory_deny_search_depth = 3
# settings = "/absolute/or/relative/path/to/srt-settings.json"

[[jobs]]
name = "deploy"
engine = "sandbox"
sandbox_debug = false
sandbox_allowed_domains = ["registry.npmjs.org", "*.npmjs.org"]
sandbox_ignore_violations = { "*" = [".cargo/registry/src"] }
sandbox_enable_weaker_nested_sandbox = true
sandbox_mandatory_deny_search_depth = 6

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
- `sandbox`

Additional engine-level controls:

- `preserve_runtime_objects`
  - default: `false`
  - when `true`, Opal keeps job/service runtime objects for inspection instead of cleaning them up automatically after successful job completion
- `map_host_user`
  - default: `true`
  - when `true`, Opal injects `OPAL_HOST_UID` and `OPAL_HOST_GID` into the runtime env and defaults `image:docker:user` / `services:docker:user` to `${OPAL_HOST_UID}:${OPAL_HOST_GID}` when those keys are unset
  - set `map_host_user = false` to opt out when a pipeline needs the engine's default container user behavior
  - explicit `image:docker:user` and `services:docker:user` values still win over this fallback

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
  - `engine`: override only this job's execution engine (`container`, `docker`, `podman`, `nerdctl`, `orbstack`, or `sandbox`)
  - `arch`: override job architecture/platform selection
  - `privileged`: request privileged containers on engines that support it
  - `cap_add`: add Linux capabilities on engines that support it
  - `cap_drop`: drop Linux capabilities on engines that support it
- Engine behavior:
  - `docker`, `podman`, `nerdctl`, `orbstack`: support `privileged`, `cap_add`, and `cap_drop`
  - Apple `container`: supports `arch`, but fails explicitly if `privileged` or capability flags are requested
  - `sandbox`: runs commands through the local Anthropic `srt` CLI, does not use container `image` flags directly, and fails explicitly if `privileged` or capability flags are requested
  - when the global run engine is `sandbox`, Opal skips pre-run registry login because the sandbox path does not pull container images directly
- Service behavior:
  - Job services continue to use the run/global engine.
  - Per-job `engine` overrides do not currently re-home services to that per-job engine.
  - A job configured with `engine = "sandbox"` fails fast when services are present, because that combination is not wired yet.
- Planning/execution interaction:
  - `opal plan --job <name>` and `opal run --job <name>` filter the execution plan first.
  - Any matching `[[jobs]]` override still applies to the selected job instances.

Add more `[engine.<name>]` tables in the future to tune other runtimes.

## Global injected env defaults

Use a root-level `[env]` table to inject Opal-only environment defaults into every job without changing `.gitlab-ci.yml`:

```toml
[env]
RUNNER_BOOTSTRAP = "enabled"
RUNNER_INIT_SCRIPT = "/opal/bootstrap/init.sh"
RUNNER_WORKDIR = "$HOME/opal-runner"
```

Behavior and precedence:

- `[env]` entries are injected by Opal for all jobs as local runner defaults.
- Values support the same shell-style expansion Opal uses elsewhere (for example `$HOME` or `${HOME}`).
- `--env` passthrough values take precedence over conflicting `[env]` keys.
- Pipeline variables from `.gitlab-ci.yml` (`default:variables` and job-level `variables`) still override injected defaults.
- This is Opal runtime behavior only; it does not add any GitLab YAML keyword.

## Runner bootstrap pre-step

Use `[bootstrap]` to run an Opal-only pre-pipeline setup step and inject runner-like assets before jobs execute.

```toml
[bootstrap]
enabled = true
command = "bash .opal/bootstrap/prepare-runner.sh"
env_file = "bootstrap/generated.env"

[bootstrap.env]
RUNNER_HELPER = "/opal/bootstrap/scripts/helper.sh"

[[bootstrap.mounts]]
host = "bootstrap/scripts"
container = "/opal/bootstrap/scripts"
read_only = true
```

Bootstrap behavior:

- `command`: runs once before job execution starts, from the repository workdir.
- `env_file`: optional dotenv file loaded after the command (useful when the bootstrap script computes values dynamically).
- `bootstrap.env`: additional static env vars injected into every job.
- `bootstrap.mounts`: host paths mounted into every job container, so you can expose local runner helper scripts/files.
- `env_file` and `bootstrap.mounts.host` are resolved relative to the directory containing `.opal/config.toml`.
- Mounted `container` paths must be absolute.
- This is Opal runtime behavior only; `.gitlab-ci.yml` stays unchanged.

### Sandbox settings

Use `[sandbox]` to configure defaults for jobs that run with `engine = "sandbox"`:

- `settings`: optional path to an `srt` JSON settings file.
- `debug`: optional boolean; when `true`, Opal adds `--debug` to `srt`.
- `allowed_domains`: optional list; emitted as `network.allowedDomains` when Opal generates per-job `srt` settings.
  Pattern notes: wildcard entries such as `*.github.com` are supported by `srt`, but overly broad patterns (for example `*.com`) are rejected by `srt` validation.
- `denied_domains`: optional list; emitted as `network.deniedDomains`.
- `allow_unix_sockets`: optional list; emitted as `network.allowUnixSockets`.
- `allow_all_unix_sockets`: optional boolean; emitted as `network.allowAllUnixSockets`.
- `allow_local_binding`: optional boolean; emitted as `network.allowLocalBinding`.
- `deny_read`: optional list; emitted as `filesystem.denyRead`.
- `allow_write`: optional list; emitted as `filesystem.allowWrite`.
- `deny_write`: optional list; emitted as `filesystem.denyWrite`.
- `ignore_violations`: optional map; emitted as `ignoreViolations`.
- `enable_weaker_nested_sandbox`: optional boolean; emitted as `enableWeakerNestedSandbox`.
- `mandatory_deny_search_depth`: optional integer; emitted as `mandatoryDenySearchDepth`.

Opal also auto-injects a sandbox ignore-violations path for each sandbox job’s workspace Cargo source registry (`<job-workspace>/.cargo/registry/src`) so Rust toolchains can unpack crates that include `.gitmodules`.

Per-job overrides under `[[jobs]]`:

- `sandbox_settings`
- `sandbox_debug`
- `sandbox_allowed_domains`
- `sandbox_denied_domains`
- `sandbox_allow_unix_sockets`
- `sandbox_allow_all_unix_sockets`
- `sandbox_allow_local_binding`
- `sandbox_deny_read`
- `sandbox_allow_write`
- `sandbox_deny_write`
- `sandbox_ignore_violations`
- `sandbox_enable_weaker_nested_sandbox`
- `sandbox_mandatory_deny_search_depth`

Rules:

- You cannot combine `sandbox_settings` with inline sandbox keys for the same resolved job (`sandbox_*` network/filesystem/policy fields).
- If global run engine is `sandbox`, registry logins are skipped (sandbox does not pull images directly).

## AI settings

AI troubleshooting configuration is documented separately in `docs/ai-config.md`.

Use that page for:

- backend selection (`ollama`, `claude`, `codex`)
- clean backend-specific examples for Claude Code, Codex, and Ollama
- prompt-template file overrides
- Ollama host/model settings
- Claude Code command/model settings
- Codex command/model settings
- analysis storage behavior

The current prompt-template placeholders are documented in `docs/ai.md`.

## Registry authentication

Each `[[registry]]` entry describes how to log into a container registry **before** jobs start:

- `server`: registry host (e.g., `registry.gitlab.com`).
- `username`: login name or token.
- `password` / `password_env`: either supply a literal password or point to an environment variable that contains it. One must be present.
- `engines`: optional list restricting the entry to specific engines (`container`, `docker`, `podman`, `nerdctl`, `orbstack`, `sandbox`). Leave empty to apply everywhere.
- `scheme`: optional for Apple’s `container registry login`.

Opal pipes the resolved credentials into the correct CLI (`container registry login`, `docker login`, etc.), so you no longer have to run those manually on the host.

Use this for private images in:

- `image` (job and default image)
- `services` image references
- `include:project` fetches and `needs:project` artifact retrieval (when credentials are available via other config)

Because this runs from `.opal/config.toml` before execution planning, you do not need to alter `.gitlab-ci.yml` to inject login steps for private registries.

> Store secrets outside of version control. When committing a project-level `config.toml`, prefer `password_env` so tokens come from CI variables or local shell env vars.
