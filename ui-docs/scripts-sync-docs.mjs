import fs from 'node:fs/promises';
import path from 'node:path';
import { marked } from 'marked';
import { createHighlighter } from 'shiki';

const repoRoot = path.resolve(process.cwd(), '..');
const docsDir = path.join(repoRoot, 'docs');
const outputDir = path.join(process.cwd(), 'src', 'lib', 'generated');
const outputFile = path.join(outputDir, 'docs.json');
const releaseFile = path.join(outputDir, 'release.json');

const preferredOrder = ['index', 'install', 'quickstart', 'pipeline', 'plan', 'config', 'ai-config', 'ai', 'gitlab-parity', 'ui'];

const cargoToml = await fs.readFile(path.join(repoRoot, 'Cargo.toml'), 'utf8');
const version = cargoToml.match(/^version = "([^"]+)"$/m)?.[1];
const repository = cargoToml.match(/^repository = "([^"]+)"$/m)?.[1];

if (!version || !repository) {
  throw new Error('failed to read version/repository from Cargo.toml');
}

const releaseTag = `v${version}`;
const releasesBaseUrl = `${repository}/releases/download/${releaseTag}`;

const releaseMeta = {
  version,
  tag: releaseTag,
  repository,
  releasesUrl: `${repository}/releases`,
  assets: {
    macosArm64: `${releasesBaseUrl}/opal-${version}-aarch64-apple-silicon.tar.gz`,
    linuxArm64: `${releasesBaseUrl}/opal-${version}-aarch64-unknown-linux-gnu.tar.gz`,
    linuxAmd64: `${releasesBaseUrl}/opal-${version}-x86_64-unknown-linux-gnu.tar.gz`
  }
};

const tokenMap = new Map([
  ['{{release_version}}', version],
  ['{{release_tag}}', releaseTag],
  ['{{github_repository_url}}', repository],
  ['{{github_releases_url}}', releaseMeta.releasesUrl],
  ['{{release_asset_url_macos_arm64}}', releaseMeta.assets.macosArm64],
  ['{{release_asset_url_linux_arm64}}', releaseMeta.assets.linuxArm64],
  ['{{release_asset_url_linux_amd64}}', releaseMeta.assets.linuxAmd64]
]);

function applyDocTokens(markdown) {
  let output = markdown;
  for (const [token, value] of tokenMap) {
    output = output.split(token).join(value);
  }
  return output;
}

function stripInlineMarkdown(value) {
  return value
    .replace(/`([^`]+)`/g, '$1')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/__([^_]+)__/g, '$1')
    .replace(/\*([^*]+)\*/g, '$1')
    .replace(/_([^_]+)_/g, '$1')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .trim();
}

function titleFromMarkdown(markdown, fallback) {
  const match = markdown.match(/^#\s+(.+)$/m);
  return match ? stripInlineMarkdown(match[1]) : fallback;
}

function summaryFromMarkdown(markdown) {
  const lines = markdown
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith('#') && !line.startsWith('```'));
  return lines[0] ?? '';
}

function slugFromFilename(filename) {
  return filename.replace(/\.md$/i, '');
}

function slugifyHeading(text) {
  return text.toLowerCase().replace(/[^a-z0-9\s-]/g, '').trim().replace(/\s+/g, '-');
}

function collectHeadings(tokens) {
  return tokens
    .filter((token) => token.type === 'heading' && token.depth <= 3)
    .map((token) => ({ depth: token.depth, text: token.text, id: slugifyHeading(token.text) }));
}

function inferInlineLanguage(raw) {
  const text = raw.trim();
  if (!text) return 'text';
  if (/^[a-z0-9_.-]+(?::[a-z0-9_.-]+)+:?$/i.test(text) || /^[a-z0-9_.-]+:$/.test(text)) {
    return 'yaml';
  }
  if (/^\[[^\]]+\]$/.test(text) || text.endsWith('.toml')) {
    return 'toml';
  }
  if (
    /^(opal|cargo|npm|wrangler|docker|podman|nerdctl|ghostty|bash|sh)\b/.test(text) ||
    /^--?[a-zA-Z0-9_-]+/.test(text) ||
    /^\$?[A-Z][A-Z0-9_]+(?:_FILE)?$/.test(text) ||
    text.includes('/') ||
    text.includes('*.') ||
    text.includes('??') ||
    text.includes('.md') ||
    text.includes('.yml') ||
    text.includes('.yaml') ||
    text.includes('.sh')
  ) {
    return 'bash';
  }
  return 'text';
}

const highlighter = await createHighlighter({
  themes: ['github-light', 'github-dark'],
  langs: ['bash', 'sh', 'toml', 'yaml', 'yml', 'rust', 'json', 'typescript', 'javascript', 'text']
});

function dualThemeHtml(code, lang) {
  return highlighter.codeToHtml(code, {
    lang,
    themes: {
      light: 'github-light',
      dark: 'github-dark'
    }
  });
}

function toInlineShiki(raw, lang) {
  const block = dualThemeHtml(raw, lang);
  return block
    .replace(/^<pre class="shiki/, '<span class="shiki-inline shiki')
    .replace(/ tabindex="0"/g, '')
    .replace(/><code>/, '><code>')
    .replace(/<\/code><\/pre>\s*$/, '</code></span>');
}

function createRenderer() {
  const renderer = new marked.Renderer();
  renderer.heading = ({ tokens, depth }) => {
    const text = tokens.map((token) => token.raw).join('');
    const id = slugifyHeading(text);
    const inner = marked.Parser.parseInline(tokens, { renderer });
    return `<h${depth} id="${id}"><a class="heading-anchor" href="#${id}" aria-label="Link to ${stripInlineMarkdown(text)}">#</a>${inner}</h${depth}>`;
  };
  renderer.code = ({ text, lang }) => dualThemeHtml(text, (lang || 'text').toLowerCase());
  renderer.codespan = ({ text }) => toInlineShiki(text, inferInlineLanguage(text));
  return renderer;
}

function postprocessHtml(html) {
  return html.replace(
    /<p><a href="https:\/\/asciinema\.org\/a\/([^"/]+)"><img src="https:\/\/asciinema\.org\/a\/\1\.svg" alt="asciicast"><\/a><\/p>/g,
    (_match, id) =>
      `<div class="asciinema-embed"><iframe src="https://asciinema.org/a/${id}/iframe" loading="lazy" title="Asciinema recording ${id}" allowfullscreen></iframe></div>`
  );
}

const entries = (await fs.readdir(docsDir))
  .filter((name) => name.endsWith('.md'))
  .sort((a, b) => {
    const sa = slugFromFilename(a);
    const sb = slugFromFilename(b);
    const ia = preferredOrder.indexOf(sa);
    const ib = preferredOrder.indexOf(sb);
    if (ia !== -1 || ib !== -1) return (ia === -1 ? Number.MAX_SAFE_INTEGER : ia) - (ib === -1 ? Number.MAX_SAFE_INTEGER : ib);
    return sa.localeCompare(sb);
  });

const docs = [];
for (const filename of entries) {
  const filePath = path.join(docsDir, filename);
  const markdown = applyDocTokens(await fs.readFile(filePath, 'utf8'));
  const slug = slugFromFilename(filename);
  const title = titleFromMarkdown(markdown, slug);
  const tokens = marked.lexer(markdown);
  const headings = collectHeadings(tokens);
  const html = postprocessHtml(
    await marked.parse(markdown, { renderer: createRenderer(), headerIds: false, mangle: false })
  );
  docs.push({ slug, title, summary: summaryFromMarkdown(markdown), headings, html });
}

await fs.mkdir(outputDir, { recursive: true });
await fs.writeFile(outputFile, JSON.stringify(docs, null, 2));
await fs.writeFile(releaseFile, JSON.stringify(releaseMeta, null, 2));
console.log(`generated ${docs.length} docs into ${path.relative(process.cwd(), outputFile)}`);
