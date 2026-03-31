# Opal Docs UI

This is a Cloudflare Workers + SvelteKit docs site for Opal.

It compiles the repository markdown under `../docs/` into a route-based documentation site with:

- light/dark theme support
- sidebar navigation and search
- per-page table of contents
- Shiki highlighting for fenced code blocks and inline markdown code spans

## Development

```sh
npm install
npm run dev
```

`npm run dev` regenerates the markdown data before starting Vite.

## Build

```sh
npm run build
```

## Deploy

```sh
npm run deploy
```

The Worker is configured for the custom domain:

```text
opal.cloudflavor.io
```

## Generated content

- `scripts-sync-docs.mjs` reads `../docs/*.md`
- it generates `src/lib/generated/docs.json`
- it refreshes `src/lib/generated/release.json` only when `CI_COMMIT_TAG` or `GIT_COMMIT_TAG` is explicitly set; ordinary branch builds reuse the checked-in release metadata
- the Svelte routes render that generated data

If you change the markdown docs, rerun any of:

```sh
npm run docs:sync
npm run dev
npm run build
```
