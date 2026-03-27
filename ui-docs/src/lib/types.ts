export type DocHeading = {
  depth: number;
  text: string;
  id: string;
};

export type DocPage = {
  slug: string;
  title: string;
  summary: string;
  headings: DocHeading[];
  html: string;
};
