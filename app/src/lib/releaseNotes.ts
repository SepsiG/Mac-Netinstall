// Release notes (GitHub Markdown) as plain-text blocks: headings, list
// items and paragraphs. Never HTML: tags are dropped, links keep only
// their text, and the blocks are rendered as text by Svelte.

export interface NoteBlock {
  kind: "heading" | "item" | "text";
  text: string;
}

/** Most blocks shown. */
export const MAX_BLOCKS = 500;

/** Inline Markdown and HTML reduced to its text. */
function inline(s: string): string {
  return s
    .replace(/<[^>]*>/g, "")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/(\*\*|__)(.+?)\1/g, "$2")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

export function noteBlocks(md: string): NoteBlock[] {
  const out: NoteBlock[] = [];
  let para: string[] = [];
  const push = (kind: NoteBlock["kind"], raw: string) => {
    const text = inline(raw);
    if (text && out.length < MAX_BLOCKS) out.push({ kind, text });
  };
  const flush = () => {
    if (para.length) push("text", para.join(" "));
    para = [];
  };
  for (const line of md.split(/\r?\n/)) {
    const t = line.trim();
    const heading = /^#{1,6}\s+(.*)$/.exec(t);
    const item = /^(?:[-*+]|\d+[.)])\s+(.*)$/.exec(t);
    if (!t) {
      flush();
    } else if (heading) {
      flush();
      push("heading", heading[1]!);
    } else if (item) {
      flush();
      push("item", item[1]!);
    } else if (/^<[^>]*>$/.test(t) && !inline(t)) {
      flush(); // a tag on its own line
    } else {
      if (/^<[^>]*>/.test(t)) flush();
      para.push(t);
      if (/<\/[^>]*>$/.test(t)) flush();
    }
  }
  flush();
  return out;
}
