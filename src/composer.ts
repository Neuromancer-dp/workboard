import type { NewAttachment } from './api';
import { prepareImage } from './image';
import { approxTokens, extractPdf } from './pdf';
import { h, button } from './ui';

export interface Pending {
  kind: 'image' | 'pdf';
  name: string;
  mime: string;
  dataUrl?: string;
  b64?: string;
  text?: string;
  pages?: number;
  truncated?: boolean;
}

/** Attachment tray: images and PDFs from the + button, drag-and-drop and paste. */
export class Composer {
  items: Pending[] = [];
  visionOK = true;

  constructor(
    private readonly tray: HTMLElement,
    private readonly attachBtn: HTMLElement,
    private readonly getCtx: () => number,
    private readonly onError: (msg: string) => void,
  ) {}

  async addFiles(files: File[]): Promise<void> {
    for (const f of files) {
      try {
        if (f.type.startsWith('image/')) {
          const { dataUrl, b64 } = await prepareImage(f);
          this.items.push({ kind: 'image', name: f.name || 'image', mime: 'image/jpeg', dataUrl, b64 });
        } else if (f.type === 'application/pdf' || /\.pdf$/i.test(f.name)) {
          const r = await extractPdf(f);
          this.items.push({ kind: 'pdf', name: f.name, mime: 'application/pdf', text: r.text, pages: r.pages, truncated: r.truncated });
        } else {
          this.onError(`${f.name || 'file'}: only images and PDFs can be attached.`);
        }
      } catch (e) {
        this.onError(e instanceof Error && e.message ? e.message : `${f.name}: could not be read.`);
      }
      this.render();
    }
  }

  /** Hand the pending attachments to a send and empty the tray. */
  take(): Pending[] {
    const t = this.items;
    this.items = [];
    this.render();
    return t;
  }
  restore(items: Pending[]): void {
    this.items = [...items, ...this.items];
    this.render();
  }

  static payload(items: Pending[]): NewAttachment[] {
    return items.map((a) =>
      a.kind === 'image'
        ? { kind: 'image', name: a.name, mime: a.mime, dataB64: a.b64 }
        : { kind: 'pdf', name: a.name, mime: a.mime, text: a.text, pages: a.pages, truncated: a.truncated },
    );
  }

  /** Total PDF tokens vs the context window; the history and the answer need room too. */
  pdfTokens(): number {
    return this.items.reduce((n, a) => n + (a.kind === 'pdf' ? approxTokens(a.text?.length ?? 0) : 0), 0);
  }

  render(): void {
    this.tray.textContent = '';
    const over = this.pdfTokens() > this.getCtx() * 0.8;
    this.items.forEach((a, i) => {
      const chip = h('div', 'chip');
      if (a.kind === 'image' && a.dataUrl) {
        const im = h('img');
        im.src = a.dataUrl;
        im.alt = '';
        chip.appendChild(im);
        if (!this.visionOK) chip.classList.add('warn');
      }
      chip.appendChild(h('span', 'n', a.name));
      const meta = h('span', 'meta');
      if (a.kind === 'pdf') {
        const tok = approxTokens(a.text?.length ?? 0);
        meta.textContent = `${a.pages}p ~${(tok / 1000).toFixed(1)}k tok${a.truncated ? ' (cut)' : ''}`;
        if (over) {
          chip.classList.add('warn');
          meta.textContent += ' · over 80% of ctx';
        }
      } else if (!this.visionOK) {
        meta.textContent = 'model has no vision, not sent';
      }
      chip.appendChild(meta);
      chip.appendChild(button('×', () => { this.items.splice(i, 1); this.render(); }, '', 'Remove'));
      this.tray.appendChild(chip);
    });
    this.attachBtn.title = this.visionOK ? 'Attach images or PDFs' : 'Attach PDFs (this model has no vision, images are kept but not sent)';
  }
}
