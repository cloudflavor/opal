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

## Generated content

- `scripts-sync-docs.mjs` reads `../docs/*.md`
- it generates `src/lib/generated/docs.json`
- the Svelte routes render that generated data

If you change the markdown docs, rerun any of:

```sh
npm run docs:sync
npm run dev
npm run build
```
