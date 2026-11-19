import docs from '$lib/generated/docs.json';
import type { RequestHandler } from './$types';

const SITE_ORIGIN = 'https://cloudflavor.io';
const SITE_BASE = '/opal';

/** Where this app's own path now lives on the consolidated site. */
function consolidated(path: string): string {
  return path === '/' ? `${SITE_BASE}/` : `${SITE_BASE}${path.replace(/\/$/, '')}/`;
}

export const prerender = true;

export const GET: RequestHandler = async () => {
  const now = new Date().toISOString();
  const paths = ['/', ...docs.map((doc) => (doc.slug === 'index' ? '/' : `/docs/${doc.slug}`))];
  const uniquePaths = Array.from(new Set(paths));

  const urlset = uniquePaths
    .map((path) => {
      const loc = new URL(consolidated(path), SITE_ORIGIN).toString();
      const priority = path === '/' ? '1.0' : '0.8';
      return [
        '<url>',
        `<loc>${loc}</loc>`,
        `<lastmod>${now}</lastmod>`,
        '<changefreq>weekly</changefreq>',
        `<priority>${priority}</priority>`,
        '</url>'
      ].join('');
    })
    .join('');

  const xml = `<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${urlset}</urlset>`;

  return new Response(xml, {
    headers: {
      'Content-Type': 'application/xml; charset=utf-8',
      'Cache-Control': 'public, max-age=3600'
    }
  });
};
