# Storage And Local State

This page documents where Opal stores local state, how per-job workspaces are prepared, how cache and artifacts are laid out on disk, and how project-level `.opal` content interacts with global configuration.

## `OPAL_HOME`

Opal stores runtime state under `OPAL_HOME`.

Resolution rules:

- If `OPAL_HOME` is set and absolute, Opal uses it directly.
- If `OPAL_HOME` is set and relative, Opal resolves it relative to the current working directory.
- If `OPAL_HOME` is unset, Opal defaults to:

```text
~/.opal
```

## Directory layout

Under `OPAL_HOME`, Opal stores:

```text
$OPAL_HOME/
├─ <run-id>/
│  ├─ logs/
│  ├─ scripts/
│  ├─ workspaces/
│  └─ <job-slug>/
│     ├─ artifacts/
│     └─ dependencies/
│     └─ runtime/
├─ cache/
├─ resource-groups/
├─ history.json
└─ config.toml
```

Important paths:

- per-run session root:
  - `$OPAL_HOME/<run-id>/`
- logs for a run:
  - `$OPAL_HOME/<run-id>/logs/`
- generated shell scripts for a run:
  - `$OPAL_HOME/<run-id>/scripts/`
- copied per-job workspaces:
  - `$OPAL_HOME/<run-id>/workspaces/<job-slug>/`
- per-job runtime inspection summaries:
  - `$OPAL_HOME/<run-id>/<job-slug>/runtime/inspect.txt`
- persistent local cache root:
  - `$OPAL_HOME/cache/`
- cross-run local resource-group locks:
  - `$OPAL_HOME/resource-groups/`
- pipeline history database:
  - `$OPAL_HOME/history.json`

## Workspace preparation

Opal prepares a per-job workspace snapshot from your current working tree.

What this means:

- Opal does not force a fresh Git clone/fetch/clean cycle for each local job.
- Dirty tracked edits in your current repo are included.
- The repository `.git` directory is copied too, so Git-aware local behavior still works.

What gets filtered out:

- Git-ignored paths, including nested ignore rules
- generated/runtime-heavy directories such as:
  - `target/`
  - `tests-temp/`
  - `.opal/`
  - `node_modules/`
  - `.svelte-kit/`
  - `.wrangler/`
  - `.output/`
  - `.vercel/`
  - `.netlify/`
  - `build/`

This is intentional. Opal is meant to run your pipeline against the working tree you are actively editing, while still avoiding obvious local junk.

## Artifacts

Artifacts are stored per job under the current run session.

Layout:

```text
$OPAL_HOME/<run-id>/<job-slug>/artifacts/
```

Behavior:

- `artifacts.paths` are copied into that directory after the job completes.
- `artifacts.exclude` is applied while collecting declared artifact paths.
- `artifacts.untracked` is collected from the copied job workspace.
- `artifacts:reports:dotenv` is copied into the same artifact tree and later reloaded where supported.

Dependency staging:

- downstream jobs do not write directly into another job’s artifact directory
- Opal stages dependency artifacts under:

```text
$OPAL_HOME/<run-id>/<job-slug>/dependencies/
```

and mounts or stages only the subset needed by the consumer job.

## Cache

Persistent cache data lives under:

```text
$OPAL_HOME/cache/
```

Behavior:

- each resolved cache key gets its own directory under the cache root
- `fallback_keys` are checked in order when the primary key is missing
- `key:files` and `key:prefix` are resolved against the workspace snapshot

Policy behavior:

- `pull`
  - restores into a staged per-job cache location
  - job writes do not mutate the persistent shared cache entry
- `push`
  - prepares a writable persistent cache entry for upload/update only
- `pull-push`
  - restores from the persistent key or fallback and then writes back to that persistent entry

Per-job staging also uses:

```text
$OPAL_HOME/<run-id>/cache-staging/
```

for staged pull-only cache copies.

## Toolchain homes in language images

Be careful when redirecting language toolchain homes into the project workspace.

For Rust images in particular:

- `CARGO_HOME` is commonly safe to point at `"$CI_PROJECT_DIR/.cargo"` when you want a local registry and crate cache.
- `RUSTUP_HOME` is different: official Rust images already contain a toolchain under the image default rustup location.
- If you override `RUSTUP_HOME` to something like `"$CI_PROJECT_DIR/.rustup"`, you can accidentally hide the bundled toolchain from `rustc`, `cargo`, and `rustup`.

Why this often appears only after a tag or new branch:

- many pipelines key caches from `CI_COMMIT_REF_SLUG`
- GitLab defines `CI_COMMIT_REF_SLUG` from the current branch or tag ref name
- a new branch or tag therefore starts with a cold cache unless another run already populated it
- if that cold cache also becomes your new `RUSTUP_HOME`, the container can suddenly fail with:

```text
error: rustup could not choose a version of rustc to run
```

Recommended pattern for Rust images:

- leave `RUSTUP_HOME` unset unless you intentionally bootstrap a toolchain into that custom location
- cache `CARGO_HOME` and `target/`
- if you do need a custom `RUSTUP_HOME`, run an explicit bootstrap step such as `rustup default stable` or your pinned toolchain install before calling `rustc` or `cargo`

## History

Opal records completed runs in:

```text
$OPAL_HOME/history.json
```

Each history entry records:

- run id
- finished timestamp
- pipeline status
- per-job:
  - name
  - stage
  - status
  - log hash
  - log path (when available)
  - artifact directory
  - artifact list
  - cache metadata
  - main job container name (when recorded)
  - service network name (when recorded)
  - service container names (when recorded)

This is what powers `opal view` and the run-history sidebar in the TUI.

## AI analysis output

When AI troubleshooting is enabled and analysis saving is turned on:

```toml
[ai]
save_analysis = true
```

Opal stores saved AI output under:

```text
$OPAL_HOME/<run-id>/<job-slug>/analysis/
```

Current backend-specific filenames include:

- `ollama.md`
- `codex.md`

This is where the final saved analysis text is written after a successful provider run.

## Runtime object cleanup

By default, Opal cleans up runtime objects after successful job completion:

- main job containers
- service containers
- per-job service networks

You can override that with config:

```toml
[engine]
preserve_runtime_objects = true
```

When enabled, Opal keeps those runtime objects for post-run inspection and records their names into job history so they can be surfaced in `opal view`.

Opal also writes a normalized runtime summary file per job at:

```text
$OPAL_HOME/<run-id>/<job-slug>/runtime/inspect.txt
```

That file is intended to be the easiest single place to inspect the recorded runtime/container details from `opal view`.

## Resource groups

Local `resource_group` locking is stored under:

```text
$OPAL_HOME/resource-groups/
```

This is how Opal serializes matching jobs across separate local runs on the same machine.

## Project-level `.opal` directory

Inside a repository, `.opal/` is used for project-scoped Opal inputs.

Supported project-level files today:

- `.opal/config.toml`
  - project-local runtime/config overrides
- `.opal/env/`
  - preferred secret directory
- `.opal/env` as a file
  - supported as dotenv-style secret input

Legacy compatibility:

- Opal still supports direct secret files under `.opal/` when their filenames are valid environment variable names.
- `.opal/env/` takes precedence over those legacy direct `.opal/` secret files.

## Configuration precedence

Opal loads and merges configuration from these paths in order:

1. `<workdir>/.opal/config.toml`
2. `$OPAL_HOME/config.toml`
3. `$XDG_CONFIG_HOME/opal/config.toml`

Earlier entries override later ones.

That means:

- project-level `.opal/config.toml` overrides your global/user defaults
- `OPAL_HOME/config.toml` can act as a machine-local override layer
- the XDG config file is the broadest user default layer

This is the mechanism that lets you keep global defaults while still overriding them per project.
