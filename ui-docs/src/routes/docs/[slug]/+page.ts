import { error } from '@sveltejs/kit';
import docs from '$lib/generated/docs.json';
import type { DocPage } from '$lib/types';

const allDocs = docs as DocPage[];

export const load = ({ params }) => {
  const index = allDocs.findIndex((doc) => doc.slug === params.slug);
  if (index === -1) {
    throw error(404, 'Document not found');
  }
  return {
    doc: allDocs[index],
    previous: index > 0 ? allDocs[index - 1] : null,
    next: index < allDocs.length - 1 ? allDocs[index + 1] : null
  };
};
