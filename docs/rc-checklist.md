# RC Checklist

This checklist is the bar for cutting a release candidate.

## Supported local engines

- macOS: `container`, `docker`, `orbstack`, `podman`
- Linux: `docker`, `podman`, `nerdctl`

`nerdctl` is not treated as a first-class macOS host engine.

## Validation gates

- `opal plan` works for the supported local GitLab subset.
- `opal run --no-tui` can run the repository pipeline on a clean checkout.
- The full fixture harness passes on each supported macOS engine.
- Supported feature areas have fixture-level or focused unit coverage.
- User docs match the actual supported engine set and parity scope.

## Known acceptable partials for RC

- full `include:project` parity
- remote/template/component include support
- additional artifact report types beyond `reports:dotenv`
- full GitLab Runner control-plane semantics
- `environment:kubernetes`
- runner-tag scheduling semantics

These remain post-RC work unless a supported local workflow depends on them.
