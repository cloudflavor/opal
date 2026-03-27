import docs from '$lib/generated/docs.json';
import type { DocPage } from '$lib/types';

export const load = () => ({ docs: docs as DocPage[] });
