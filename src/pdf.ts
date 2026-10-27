export const MAX_PDF_CHARS = 60000;

export interface PdfText {
  text: string;
  pages: number;
  truncated: boolean;
}

export class NoTextError extends Error {}

/** Approximate tokens the same way the prototype did. */
export const approxTokens = (chars: number): number => Math.round(chars / 4);

/**
 * Extract text with the locally bundled pdf.js. The library and its worker are separate chunks that only load
 * the first time a PDF is attached, so they cost nothing at startup.
 */
export async function extractPdf(file: File): Promise<PdfText> {
  const pdfjs = await import('pdfjs-dist');
  const worker = (await import('pdfjs-dist/build/pdf.worker.min.mjs?url')).default;
  pdfjs.GlobalWorkerOptions.workerSrc = worker;

  const data = new Uint8Array(await file.arrayBuffer());
  const task = pdfjs.getDocument({ data, useWasm: false, verbosity: 0, disableAutoFetch: true });
  let doc;
  try {
    doc = await task.promise;
  } catch (e) {
    if ((e as { name?: string }).name === 'PasswordException') throw new Error(`${file.name}: password-protected PDFs are not supported.`);
    throw new Error(`${file.name}: could not read this PDF (${(e as Error).message}).`);
  }
  try {
    let text = '';
    for (let p = 1; p <= doc.numPages && text.length < MAX_PDF_CHARS; p++) {
      const page = await doc.getPage(p);
      const tc = await page.getTextContent();
      let line = '';
      for (const it of tc.items) {
        if (!('str' in it)) continue;
        line += it.str + (it.hasEOL ? '\n' : ' ');
      }
      text += line.replace(/[ \t]+\n/g, '\n').trim() + '\n\n';
      page.cleanup();
    }
    text = text.trim();
    if (!text) throw new NoTextError(`${file.name}: no selectable text (scanned PDF?). Attach its pages as images instead.`);
    const truncated = text.length > MAX_PDF_CHARS;
    return { text: truncated ? text.slice(0, MAX_PDF_CHARS) : text, pages: doc.numPages, truncated };
  } finally {
    void task.destroy();
  }
}
