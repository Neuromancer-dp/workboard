/** Tiny DOM helpers, modal dialogs and the notice bar. */

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, cls = '', text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

export function button(label: string, onClick: (e: MouseEvent) => void, cls = '', title = ''): HTMLButtonElement {
  const b = h('button', cls, label);
  b.type = 'button';
  if (title) b.title = title;
  b.addEventListener('click', onClick);
  return b;
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // WebKitGTK can refuse the async clipboard API; fall back to a selection copy.
    const ta = h('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand('copy');
    } catch {
      ok = false;
    }
    ta.remove();
    return ok;
  }
}

/** Flash a button label ("copy" -> "copied"). */
export async function copyWithFeedback(btn: HTMLElement, text: string): Promise<void> {
  const old = btn.textContent;
  const ok = await copyText(text);
  btn.textContent = ok ? 'copied' : 'copy failed';
  setTimeout(() => (btn.textContent = old), 1200);
}

// ---------- modal ----------

const root = () => document.getElementById('modal-root') as HTMLElement;

export interface ModalHandle {
  close(): void;
  body: HTMLElement;
}

/** Generic modal. Escape or a click on the backdrop closes it; `onClose` runs once either way. */
export function openModal(title: string, build: (body: HTMLElement, close: () => void) => void, onClose?: () => void): ModalHandle {
  const scrim = h('div', 'scrim');
  const box = h('div', 'modal');
  box.setAttribute('role', 'dialog');
  box.setAttribute('aria-modal', 'true');
  box.appendChild(h('h2', '', title));
  const prevFocus = document.activeElement as HTMLElement | null;
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    document.removeEventListener('keydown', onKey, true);
    scrim.remove();
    prevFocus?.focus?.();
    onClose?.();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  };
  document.addEventListener('keydown', onKey, true);
  scrim.addEventListener('mousedown', (e) => {
    if (e.target === scrim) close();
  });
  build(box, close);
  scrim.appendChild(box);
  root().appendChild(scrim);
  (box.querySelector('[autofocus], button.primary, input, button') as HTMLElement | null)?.focus();
  return { close, body: box };
}

export function confirmDialog(opts: { title: string; message: string; confirm: string; danger?: boolean }): Promise<boolean> {
  return new Promise((resolve) => {
    let result = false;
    openModal(
      opts.title,
      (box, close) => {
        box.appendChild(h('p', '', opts.message));
        const btns = h('div', 'btns');
        btns.append(
          button('Cancel', close),
          button(opts.confirm, () => {
            result = true;
            close();
          }, opts.danger ? 'danger' : 'primary'),
        );
        box.appendChild(btns);
      },
      () => resolve(result),
    );
  });
}

// ---------- notice bar above the composer ----------

let noticeTimer = 0;
export function notice(text: string, kind: 'info' | 'err' = 'info'): void {
  const el = document.getElementById('notice') as HTMLElement;
  el.textContent = '';
  el.className = 'notice' + (kind === 'err' ? ' err' : '');
  const msg = h('span', '', text);
  const x = button('×', () => (el.hidden = true), '', 'Dismiss');
  el.append(msg, x);
  el.hidden = false;
  clearTimeout(noticeTimer);
  if (kind === 'info') noticeTimer = window.setTimeout(() => (el.hidden = true), 9000);
}
export function clearNotice(): void {
  (document.getElementById('notice') as HTMLElement).hidden = true;
}
