import type { AttachmentMeta, Source, Stats, Status } from './api';
import { api } from './api';
import { MarkdownView } from './markdown';
import { mergeThinking, splitThinking } from './think';
import { button, copyWithFeedback, h } from './ui';

export interface ScrollHost {
  /** call before changing the DOM; true if the view is pinned to the bottom */
  beforeMutate(): boolean;
  afterMutate(stick: boolean): void;
}

export function statsLine(s: Stats | null | undefined): string {
  if (!s) return '';
  const parts: string[] = [];
  if (s.tokS) parts.push(`${s.tokS.toFixed(1)} tok/s`);
  if (s.evalCount) parts.push(`${s.evalCount} tokens out`);
  if (s.promptEvalCount) parts.push(`${s.promptEvalCount} in`);
  return parts.join('  ·  ');
}

/** An attachment as shown in a message: freshly attached (has a data URL) or stored (fetched on demand). */
export interface AttView {
  kind: 'image' | 'pdf';
  name: string;
  pages?: number | null;
  dataUrl?: string;
  stored?: AttachmentMeta;
}

export class BlobUrls {
  private urls: string[] = [];
  async forAttachment(a: AttachmentMeta): Promise<string> {
    const buf = await api.attachmentBytes(a.id);
    const url = URL.createObjectURL(new Blob([buf], { type: a.mime || 'image/jpeg' }));
    this.urls.push(url);
    return url;
  }
  revokeAll(): void {
    this.urls.forEach((u) => URL.revokeObjectURL(u));
    this.urls = [];
  }
}

export function userRow(text: string, atts: AttView[], blobs: BlobUrls): HTMLElement {
  const row = h('div', 'row user');
  row.appendChild(h('div', 'who', 'you'));
  if (atts.length) {
    const th = h('div', 'thumbs');
    for (const a of atts) {
      if (a.kind === 'image') {
        const im = h('img');
        im.alt = a.name;
        im.title = a.name;
        if (a.dataUrl) im.src = a.dataUrl;
        else if (a.stored) blobs.forAttachment(a.stored).then((u) => (im.src = u), () => (im.alt = `${a.name} (could not load)`));
        th.appendChild(im);
      } else {
        th.appendChild(h('span', 'pdfchip', `${a.name} (${a.pages ?? '?'}p)`));
      }
    }
    row.appendChild(th);
  }
  if (text) row.appendChild(h('div', 'body', text));
  return row;
}

/**
 * Reveal a share of the backlog each frame: a few characters when little is waiting (steady typing),
 * proportionally more when a burst arrives so the view never lags far behind the model.
 */
export function nextReveal(text: string, shown: number): number {
  const backlog = text.length - shown;
  if (backlog <= 0) return text.length;
  let n = shown + Math.max(2, Math.ceil(backlog / 8));
  if (n < text.length) {
    const c = text.charCodeAt(n - 1);
    if (c >= 0xd800 && c <= 0xdbff) n++; // never split a surrogate pair
  }
  return Math.min(n, text.length);
}

/** One assistant reply: collapsed thinking block, markdown body, sources, notes and stats. */
export class AssistantRow {
  readonly el = h('div', 'row assistant');
  content = '';
  thinking = '';
  private live = false;
  private raf = 0;
  /** how much of `content` has been revealed on screen (smooth streaming) */
  private shown = 0;
  private details: HTMLDetailsElement | null = null;
  private summary: HTMLElement | null = null;
  private thinkBody: HTMLElement | null = null;
  private readonly body = h('div', 'body md');
  private readonly md = new MarkdownView(this.body);
  private readonly foot = h('div', 'foot');

  constructor(
    readonly messageId: number,
    model: string,
    private readonly host: ScrollHost,
  ) {
    const who = h('div', 'who');
    who.append(h('span', 'name', model || 'model'), h('span', 'sp'));
    who.appendChild(button('copy', (e) => copyWithFeedback(e.currentTarget as HTMLElement, this.answerText()), 'msgcopy', 'Copy this reply'));
    this.el.append(who, this.body, this.foot);
  }

  /** Reply text without any thinking block. */
  answerText(): string {
    return splitThinking(this.content).answer;
  }

  setLive(on: boolean): void {
    this.live = on;
    this.el.classList.toggle('live', on);
  }

  setText(content: string, thinking: string): void {
    this.content = content;
    this.thinking = thinking;
    this.shown = content.length;
    this.paint();
  }

  /** Streamed text arrives here; painting is coalesced to one pass per animation frame. */
  append(dc: string, dt: string): void {
    this.content += dc;
    this.thinking += dt;
    if (!this.raf) this.raf = requestAnimationFrame(() => this.paint());
  }

  private paint(final = false): void {
    if (this.raf) cancelAnimationFrame(this.raf);
    this.raf = 0;
    const stick = this.host.beforeMutate();
    if (final || !this.live) this.shown = this.content.length;
    else this.shown = nextReveal(this.content, this.shown);
    const s = splitThinking(this.content.slice(0, this.shown), this.live);
    this.showThinking(mergeThinking(this.thinking, s.thinking), this.live && s.open);
    this.md.update(s.answer, final || !this.live);
    this.host.afterMutate(stick);
    // bursty network chunks are drained over several frames instead of landing all at once
    if (this.live && this.shown < this.content.length) this.raf = requestAnimationFrame(() => this.paint());
  }

  private showThinking(text: string, open: boolean): void {
    if (!text) return;
    if (!this.details) {
      this.details = h('details', 'think');
      this.summary = h('summary', '', 'thinking');
      this.thinkBody = h('div', 'think-body');
      this.details.append(this.summary, this.thinkBody);
      this.el.insertBefore(this.details, this.body);
    }
    this.summary!.textContent = open ? 'thinking' : 'thought process';
    this.thinkBody!.textContent = text;
  }

  setSources(sources: Source[] | null): void {
    this.foot.querySelector('.sources')?.remove();
    if (!sources?.length) return;
    const box = h('div', 'sources');
    sources.forEach((s, i) => {
      const a = h('a', '', `[${i + 1}] ${s.title}`);
      a.href = s.url;
      a.title = s.url;
      box.appendChild(a);
    });
    this.foot.prepend(box);
  }

  private note(text: string, cls: string): void {
    this.foot.appendChild(h('div', `note ${cls}`, text));
  }

  /** Final state, from the stream or from storage. */
  finish(status: Status, stats: Stats | null): void {
    this.setLive(false);
    this.paint(true);
    for (const n of Array.from(this.foot.querySelectorAll('.note, .mstats, .errnote'))) n.remove();
    if (stats?.searchError) this.note(`web search: ${stats.searchError} (answered without it)`, 'err');
    if (status === 'error') this.foot.appendChild(h('div', 'errnote', `Error: ${stats?.error ?? 'unknown error'}`));
    if (status === 'stopped') this.note('stopped', '');
    if (status === 'interrupted' || status === 'streaming') this.note('this reply was interrupted before it finished (the app closed or crashed); the text above is what was saved', '');
    const line = statsLine(stats);
    if (line) this.foot.appendChild(h('div', 'mstats', line));
  }
}
