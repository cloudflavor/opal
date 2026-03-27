# CLI Reference

This page documents the current Opal command-line surface directly from the implementation.

## Global options

### `--log-level <level>`

- Default: `info`
- Supported values:
  - `trace`
  - `debug`
  - `info`
  - `warn`
  - `error`

Example:

```bash
opal --log-level debug plan --pipeline .gitlab-ci.yml --workdir .
```

## Commands

### `opal run`

Runs a pipeline locally.

#### Options

##### `-p, --pipeline <path>`

- Which `.gitlab-ci.yml` file to use
- Defaults to `<workdir>/.gitlab-ci.yml`

##### `-w, --workdir <path>`

- Context directory
- Defaults to the current working directory

##### `-b, --base-image <image>`

- Optional fallback image when the pipeline/job does not define one

##### `-E, --env <glob>`

- Repeatable
- Forwards selected host environment variables into every job
- Uses `globset` glob syntax

Examples:

```bash
opal run -E CI_* -E 'AWS_ACCESS_KEY_ID' -E 'APP_??'
```

##### `--max-parallel-jobs <n>`

- Default: `5`
- Maximum number of pipeline jobs Opal runs concurrently in a single invocation

##### `--trace-scripts`

- Enables shell tracing with `set -x` in generated job scripts

##### `--engine <engine>`

- Default: `auto`
- Accepted values:
  - `auto`
  - `container`
  - `docker`
  - `podman`
  - `nerdctl`
  - `orbstack`

Notes:

- On macOS, `auto` prefers `orbstack` when detected, otherwise uses Apple `container`.
- On Linux, `auto` uses `docker`.
- On macOS, `nerdctl` is treated as Linux-oriented rather than as a first-class host engine.

##### `--no-tui`

- Disables the Ratatui interface and prints plain terminal output instead

##### `--gitlab-base-url <url>`

- Optional GitLab API base URL
- Default when used with GitLab features: `https://gitlab.com`
- Also available through `OPAL_GITLAB_BASE_URL`

##### `--gitlab-token <token>`

- Optional GitLab personal access token
- Used for cross-project artifact/include features that require GitLab API access
- Also available through `OPAL_GITLAB_TOKEN`

##### `--job <name>`

- Repeatable
- Limits execution to selected jobs plus their required upstream dependency closure

Examples:

```bash
opal run --job lint
opal run --job package-linux --job deploy-summary
```

### `opal plan`

Builds and prints the evaluated execution plan without starting containers.

#### Options

##### `-p, --pipeline <path>`

- Which `.gitlab-ci.yml` file to inspect
- Defaults to `<workdir>/.gitlab-ci.yml`

##### `-w, --workdir <path>`

- Context directory
- Defaults to the current working directory

##### `--gitlab-base-url <url>`

- Optional GitLab API base URL for GitLab-backed include resolution
- Also available through `OPAL_GITLAB_BASE_URL`

##### `--gitlab-token <token>`

- Optional GitLab token for GitLab-backed include resolution
- Also available through `OPAL_GITLAB_TOKEN`

##### `--job <name>`

- Repeatable
- Limits the printed plan to selected jobs plus their required upstream dependency closure

Example:

```bash
opal plan --pipeline .gitlab-ci.yml --workdir . --job package-linux
```

### `opal view`

Opens the history/log browser for previous runs.

#### Options

##### `-w, --workdir <path>`

- Context directory
- Defaults to the current working directory

## Related environment variables

These are not command-line flags, but they change CLI/runtime behavior and are worth knowing:

- `OPAL_GITLAB_BASE_URL`
  - fallback for `--gitlab-base-url`
- `OPAL_GITLAB_TOKEN`
  - fallback for `--gitlab-token`
- `OPAL_HOME`
  - changes where Opal stores runs, logs, artifacts, cache, and history
- `OPAL_RUN_MANUAL=1`
  - makes manual jobs auto-run in contexts that respect manual-run toggling
- `OPAL_DEBUG=1`
  - enables script tracing like `--trace-scripts`

## Common examples

Run the default repo pipeline locally:

```bash
opal run --workdir . --pipeline .gitlab-ci.yml
```

Run one job plus required upstreams:

```bash
opal run --no-tui --job lint
```

Preview the evaluated DAG only:

```bash
opal plan --workdir . --pipeline .gitlab-ci.yml
```

Inspect a subgraph:

```bash
opal plan --job package-linux
```
