# Opal Configuration

Opal reads a layered `config.toml` to customize runtime behavior and automatically handle container registry authentication. Two locations are checked (earlier entries override later ones):

1. `$REPO/.opal/config.toml` – project-specific settings committed alongside your pipeline.
2. `$XDG_CONFIG_HOME/opal/config.toml` (or `~/Library/Application Support/opal/config.toml` on macOS) – user-wide defaults.

## Example

```toml
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

Add more `[engine.<name>]` tables in the future to tune other runtimes.

## Registry authentication

Each `[[registry]]` entry describes how to log into a container registry **before** jobs start:

- `server`: registry host (e.g., `registry.gitlab.com`).
- `username`: login name or token.
- `password` / `password_env`: either supply a literal password or point to an environment variable that contains it. One must be present.
- `engines`: optional list restricting the entry to specific engines (`container`, `docker`, `podman`, `nerdctl`, `orbstack`). Leave empty to apply everywhere.
- `scheme`: optional for Apple’s `container registry login`.

Opal pipes the resolved credentials into the correct CLI (`container registry login`, `docker login`, etc.), so you no longer have to run those manually on the host.

> Store secrets outside of version control. When committing a project-level `config.toml`, prefer `password_env` so tokens come from CI variables or local shell env vars.
