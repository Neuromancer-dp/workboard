import { api, errMsg } from './api';
import { button, h, notice, openModal } from './ui';
import { safeFilename } from './exporter';

/** Median frame time (ms) over `frames` animation frames, or null if the window is not being painted. */
export function measureFrameTime(frames = 50, warmup = 10): Promise<number | null> {
  return new Promise((resolve) => {
    const deltas: number[] = [];
    let last = 0;
    let n = 0;
    const tick = (t: number) => {
      if (document.hidden) return resolve(null);
      if (last && n++ >= warmup) deltas.push(t - last);
      last = t;
      if (deltas.length >= frames) {
        deltas.sort((a, b) => a - b);
        return resolve(deltas[Math.floor(deltas.length / 2)]);
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

export interface SettingsHooks {
  getPort(): string;
  getReduce(): boolean;
  setReduce(on: boolean): void;
  /** called after the port was applied; resolves once Ollama has been (re)checked */
  portChanged(): Promise<{ ok: boolean; message: string }>;
}

const today = () => new Date().toISOString().slice(0, 10);

export function openSettings(hooks: SettingsHooks): void {
  openModal('Settings', (box, close) => {
    // Ollama port
    const portField = h('div', 'field');
    const lbl = h('label', '', 'Ollama port');
    lbl.title = 'Ollama is always reached on 127.0.0.1 (this machine)';
    const port = h('input');
    port.type = 'number';
    port.min = '1';
    port.max = '65535';
    port.value = hooks.getPort();
    const status = h('div', 'status');
    const apply = button('Apply', async () => {
      status.className = 'status';
      status.textContent = 'checking…';
      try {
        await api.setSetting('ollama_port', port.value);
        const r = await hooks.portChanged();
        status.textContent = r.message;
        status.classList.toggle('err', !r.ok);
      } catch (e) {
        status.textContent = errMsg(e);
        status.classList.add('err');
      }
    });
    portField.append(lbl, port, apply);
    box.append(portField, status);
    box.appendChild(h('p', 'dim', 'If Ollama is not running when the app starts, it is launched for you and stopped again when you quit. An instance you started yourself is never stopped.'));

    // Reduce effects
    const fx = h('div', 'field');
    const fxLabel = h('label', '', 'Reduce effects (no blur, no glow animation)');
    fxLabel.title = 'Backdrop blur is expensive in WebKitGTK. Turn this on if scrolling or typing lags.';
    const cb = h('input');
    cb.type = 'checkbox';
    cb.checked = hooks.getReduce();
    cb.addEventListener('change', () => hooks.setReduce(cb.checked));
    fx.append(fxLabel, cb);
    box.appendChild(fx);

    box.appendChild(h('hr'));

    // Data
    box.appendChild(h('p', 'dim', 'All chats, attachments and settings live in one SQLite file on this machine:'));
    const where = h('code', '', '…');
    api.dataLocation().then((p) => (where.textContent = p ? `${p}/chat.db` : 'unknown'), () => (where.textContent = 'unknown'));
    const wp = h('p');
    wp.appendChild(where);
    box.appendChild(wp);
    const row = h('div', 'row-btns');
    row.append(
      button('Backup database…', async () => {
        try {
          const path = await api.pickSavePath(`chat-backup-${today()}.db`, 'SQLite database', 'db');
          if (!path) return;
          await api.backupDatabase(path);
          notice(`Database copied to ${path}`);
        } catch (e) {
          notice(`Backup failed: ${errMsg(e)}`, 'err');
        }
      }),
      button('Export all chats (JSON)…', async () => {
        try {
          const path = await api.pickSavePath(safeFilename(`chat-export-${today()}`, 'json'), 'JSON', 'json');
          if (!path) return;
          await api.exportAllJson(path);
          notice(`All chats exported to ${path}`);
        } catch (e) {
          notice(`Export failed: ${errMsg(e)}`, 'err');
        }
      }),
    );
    box.appendChild(row);
    box.appendChild(h('p', 'dim', 'Nothing leaves this machine except web searches, and only when the web toggle is on.'));

    const btns = h('div', 'btns');
    btns.appendChild(button('Close', close, 'primary'));
    box.appendChild(btns);
  });
}
