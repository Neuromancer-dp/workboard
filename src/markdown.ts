import { Marked, type Token } from 'marked';
import DOMPurify from 'dompurify';
import hljs from 'highlight.js/lib/core';
import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cpp from 'highlight.js/lib/languages/cpp';
import css from 'highlight.js/lib/languages/css';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import python from 'highlight.js/lib/languages/python';
import rust from 'highlight.js/lib/languages/rust';
import sql from 'highlight.js/lib/languages/sql';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';

// Only the common languages; aliases (js, ts, sh, html, yml, rs, c++ …) come with each definition.
const LANGS = { bash, c, cpp, css, javascript, json, python, rust, sql, typescript, xml, yaml };
for (const [name, def] of Object.entries(LANGS)) hljs.registerLanguage(name, def);

// ---------- marked ----------

const marked = new Marked({ gfm: true, breaks: false });
marked.use({
  renderer: {
    // Remote images would be a request to a third party: never emit an <img> for model output.
    image({ text }) {
      return text ? `<span class="img-alt">[image: ${esc(text)}]</span>` : '';
    },
    checkbox({ checked }) {
      return `<span class="task">${checked ? '☑' : '☐'} </span>`;
    },
  },
});

function esc(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

// ---------- sanitizer ----------

const ALLOWED_TAGS = [
  'p', 'br', 'hr', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'strong', 'b', 'em', 'i', 'del', 's', 'u', 'mark', 'sub', 'sup', 'kbd',
  'code', 'pre', 'blockquote', 'ul', 'ol', 'li', 'a', 'table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td', 'span', 'div',
  'details', 'summary', 'dl', 'dt', 'dd',
];

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (!(node instanceof Element)) return;
  if (node.tagName === 'A') node.setAttribute('rel', 'noopener noreferrer');
  // Our own UI classes must not be forgeable from model output; only code-language classes survive.
  if (node.hasAttribute('class')) {
    const ok = node.tagName === 'CODE' && /^language-[\w+#.-]+$/.test(node.getAttribute('class') ?? '');
    if (!ok) node.removeAttribute('class');
  }
});

function sanitize(html: string): DocumentFragment {
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR: ['href', 'title', 'class', 'align', 'colspan', 'rowspan', 'start'],
    ADD_URI_SAFE_ATTR: ['align', 'colspan', 'rowspan', 'start'],
    ALLOWED_URI_REGEXP: /^(?:https?:|mailto:)/i,
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: false,
    RETURN_DOM_FRAGMENT: true,
  });
}

// ---------- post-processing (after sanitizing, so nothing here comes from the model) ----------

function codeLang(code: Element | null): string {
  return /(?:^|\s)language-([\w+#.-]+)/.exec(code?.getAttribute('class') ?? '')?.[1]?.toLowerCase() ?? '';
}

function decorate(frag: DocumentFragment, highlight: boolean): void {
  for (const pre of Array.from(frag.querySelectorAll('pre'))) {
    const code = pre.firstElementChild;
    if (!code || code.tagName !== 'CODE') continue;
    const lang = codeLang(code);
    if (highlight && lang && hljs.getLanguage(lang)) {
      // hljs escapes its input; its output is spans and text only.
      code.innerHTML = hljs.highlight(code.textContent ?? '', { language: lang, ignoreIllegals: true }).value;
      code.classList.add('hljs');
    }
    const bar = document.createElement('div');
    bar.className = 'codebar';
    const label = document.createElement('span');
    label.textContent = lang || 'text';
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'copy';
    btn.textContent = 'copy';
    bar.append(label, btn);
    pre.prepend(bar);
  }
  for (const t of Array.from(frag.querySelectorAll('table'))) {
    const wrap = document.createElement('div');
    wrap.className = 'tablewrap';
    t.replaceWith(wrap);
    wrap.appendChild(t);
  }
}

/** A fenced block whose closing fence has arrived. */
function fenceClosed(raw: string): boolean {
  return /\n {0,3}(`{3,}|~{3,})[ \t]*\n?$/.test(raw);
}

export function renderToken(token: Token, highlight = true): ChildNode[] {
  const html = marked.parser([token]);
  if (!html.trim()) return [];
  const frag = sanitize(html);
  decorate(frag, highlight);
  return Array.from(frag.childNodes);
}

/** Blocks that a following block of the same kind may still merge into while text is arriving. */
function mergeable(b: { type: string; raw: string }): boolean {
  if (b.type === 'list' || b.type === 'blockquote') return true;
  return b.type === 'code' && !/^ {0,3}(`{3,}|~{3,})/.test(b.raw); // indented code; fenced blocks never merge
}

interface Block {
  raw: string;
  type: string;
  nodes: ChildNode[];
}

/**
 * Incremental markdown view for streamed text.
 *
 * Finished top-level blocks keep their DOM nodes (and their syntax highlighting); each update re-lexes only
 * from the first block that might still change. Unclosed fences render as plain code until they close.
 * `update(text, true)` does a full from-scratch pass, so the end state never depends on how the text arrived.
 */
export class MarkdownView {
  private blocks: Block[] = [];

  constructor(private readonly root: HTMLElement) {}

  /**
   * How many leading blocks can no longer change. A block is settled once a different kind of block follows it.
   * (A half-typed "  - c" lexes as its own list; it must not freeze the list before it.)
   */
  private settled(): number {
    let n = 0;
    for (let i = 0; i < this.blocks.length - 1; i++) {
      const b = this.blocks[i];
      if (b.type !== 'space') {
        let j = i + 1;
        while (j < this.blocks.length && this.blocks[j].type === 'space') j++;
        if (j >= this.blocks.length) break;
        if (this.blocks[j].type === b.type && mergeable(b)) break;
      }
      n = i + 1;
    }
    return n;
  }

  update(input: string, final = false): void {
    const text = input.replace(/\r\n?/g, '\n');
    let keep = this.settled();
    let committed = this.blocks.slice(0, keep).map((b) => b.raw).join('');
    if (final || !text.startsWith(committed)) {
      keep = 0;
      committed = '';
    }
    const tokens = marked.lexer(text.slice(committed.length));
    for (let i = keep; i < this.blocks.length; i++) for (const n of this.blocks[i].nodes) n.remove();
    this.blocks.length = keep;

    const frag = document.createDocumentFragment();
    tokens.forEach((t, i) => {
      const live = !final && i === tokens.length - 1;
      const highlight = !(live && t.type === 'code' && !fenceClosed(t.raw));
      const nodes = renderToken(t, highlight);
      this.blocks.push({ raw: t.raw, type: t.type, nodes });
      for (const n of nodes) frag.appendChild(n);
    });
    this.root.appendChild(frag);
  }

  clear(): void {
    this.root.textContent = '';
    this.blocks = [];
  }
}

/** One-shot render, used for stored messages. */
export function renderMarkdownInto(root: HTMLElement, text: string): MarkdownView {
  const v = new MarkdownView(root);
  v.update(text, true);
  return v;
}
