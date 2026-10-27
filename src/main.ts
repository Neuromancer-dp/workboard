import { api, errMsg, type ChatEvent, type Conversation, type Message, type Source } from './api';
import { Composer, type Pending } from './composer';
import { conversationToMarkdown, safeFilename } from './exporter';
import { AssistantRow, BlobUrls, userRow, type AttView, type ScrollHost } from './messages';
import { measureFrameTime, openSettings } from './settings';
import { Sidebar } from './sidebar';
import { clearNotice, copyWithFeedback, h, notice } from './ui';

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const log = $('log'), msgs = $('msgs'), input = $<HTMLTextAreaElement>('input'), sendBtn = $<HTMLButtonElement>('send'),
  modelSel = $<HTMLSelectElement>('model'), ctxSel = $<HTMLSelectElement>('ctx'), stats = $('stats'), webBtn = $<HTMLButtonElement>('web'),
  convTitle = $('convTitle');

/** The one reply being generated right now (Rust keeps it going even if the view moves elsewhere). */
interface Live {
  conversationId: number;
  messageId: number;
  title: string;
  content: string;
  thinking: string;
  sources: Source[] | null;
  searchError: string | null;
  row: AssistantRow | null;
}

let S: Record<string, string> = {};
let convId: number | null = null;
let live: Live | null = null;
let webOn = false;
let openSeq = 0;
const blobs = new BlobUrls();

const scrollHost: ScrollHost = {
  beforeMutate: () => log.scrollHeight - log.scrollTop - log.clientHeight < 80,
  afterMutate: (stick) => {
    if (stick) log.scrollTop = log.scrollHeight;
  },
};

const composer = new Composer($('tray'), $('attach'), () => Number(ctxSel.value), (m) => notice(m, 'err'));

function save(key: string, value: string): void {
  S[key] = value;
  api.setSetting(key, value).catch((e) => notice(`Could not save setting: ${errMsg(e)}`, 'err'));
}

// ---------- effects ----------

function applyReduce(on: boolean): void {
  document.body.classList.toggle('reduce', on);
}
async function autoDetectEffects(): Promise<void> {
  if (S.reduce_effects !== undefined) return; // the user (or an earlier probe) already decided
  if ((navigator.hardwareConcurrency ?? 8) <= 2) return decide(true, 'few CPU cores');
  await new Promise((r) => setTimeout(r, 1200)); // let the first paint settle
  const ms = await measureFrameTime();
  if (ms === null) return; // hidden window: try again next launch
  decide(ms > 24, `median frame ${ms.toFixed(0)} ms`);
  function decide(slow: boolean, why: string): void {
    save('reduce_effects', slow ? '1' : '0');
    applyReduce(slow);
    if (slow) notice(`Rendering looked slow (${why}), so "Reduce effects" was turned on. You can change this in Settings.`);
  }
}

// ---------- models / ollama ----------

async function refreshVision(): Promise<void> {
  composer.visionOK = true;
  if (modelSel.value) {
    try {
      composer.visionOK = await api.modelVision(modelSel.value);
    } catch {
      /* keep optimistic default */
    }
  }
  composer.render();
}

async function loadModels(): Promise<boolean> {
  const prev = modelSel.value;
  try {
    const names = await api.listModels();
    modelSel.replaceChildren(...names.map((n) => new Option(n, n)));
    const want = [prev, S.last_model].find((m) => m && names.includes(m));
    if (want) modelSel.value = want;
    if (!names.length) notice('No models found. Pull one with: ollama pull <model>', 'err');
    else void refreshVision();
    return names.length > 0;
  } catch (e) {
    notice(errMsg(e), 'err');
    return false;
  }
}

async function startOllama(): Promise<void> {
  setStatus('starting Ollama…');
  try {
    const r = await api.ollamaEnsure();
    if (r.state === 'failed') notice(r.message, 'err');
  } catch (e) {
    notice(errMsg(e), 'err');
  }
  setStatus('');
  await loadModels();
}

// ---------- view ----------

function setStatus(t: string): void {
  stats.textContent = t;
}

function showBlank(): void {
  msgs.replaceChildren();
  const b = h('div', 'blank');
  b.append(h('b', '', 'CHAT'), document.createElement('br'), document.createTextNode('Local, private, saved on this machine.'));
  msgs.appendChild(b);
}

function clearView(): void {
  live?.row && (live.row = null);
  blobs.revokeAll();
  msgs.replaceChildren();
}

function paintSend(): void {
  const here = live !== null && live.conversationId === convId;
  sendBtn.textContent = here ? 'Stop' : 'Send';
  document.body.classList.toggle('busy', live !== null);
  if (live && !here) setStatus(`● still generating in “${live.title}” — open it to watch or stop it`);
  else if (!live) setStatus('');
  sidebar.generatingId = live?.conversationId ?? null;
}

function attViews(m: Message): AttView[] {
  return m.attachments.map((a) => ({ kind: a.kind, name: a.name, pages: a.pages, stored: a }));
}

function renderStored(messages: Message[], model: string): void {
  for (const m of messages) {
    if (m.role === 'user') {
      msgs.appendChild(userRow(m.content, attViews(m), blobs));
      continue;
    }
    const row = new AssistantRow(m.id, m.stats?.model ?? model, scrollHost);
    msgs.appendChild(row.el);
    const isLive = live !== null && live.messageId === m.id;
    if (isLive && live) {
      live.row = row;
      row.setLive(true);
      row.setText(live.content, live.thinking);
      row.setSources(live.sources);
    } else {
      row.setText(m.content, m.thinking);
      row.setSources(m.sources);
      row.finish(m.status === 'streaming' ? 'interrupted' : m.status, m.stats);
    }
  }
}

async function openConversation(id: number): Promise<void> {
  const seq = ++openSeq;
  try {
    const { conversation, messages } = await api.load(id);
    if (seq !== openSeq) return;
    clearView();
    convId = id;
    sidebar.setActive(id);
    convTitle.textContent = conversation.title;
    if ([...modelSel.options].some((o) => o.value === conversation.model)) {
      if (modelSel.value !== conversation.model) {
        modelSel.value = conversation.model;
        void refreshVision();
      }
    } else if (conversation.model) {
      notice(`This chat used “${conversation.model}”, which is not installed. Pick another model to continue it.`);
    }
    if (messages.length === 0) showBlank();
    renderStored(messages, conversation.model);
    log.scrollTop = log.scrollHeight;
    save('last_conversation', String(id));
    paintSend();
  } catch (e) {
    notice(`Could not open the chat: ${errMsg(e)}`, 'err');
  }
}

function newChat(): void {
  ++openSeq;
  clearView();
  convId = null;
  sidebar.setActive(null);
  convTitle.textContent = '';
  showBlank();
  save('last_conversation', '');
  setStatus('');
  paintSend();
  input.focus();
}

// ---------- sending ----------

function handleEvent(lv: Live, e: ChatEvent): void {
  switch (e.event) {
    case 'searching':
      setStatus('searching the web…');
      break;
    case 'sources':
      lv.sources = e.sources;
      lv.row?.setSources(e.sources);
      setStatus('');
      break;
    case 'searchError':
      lv.searchError = e.message;
      notice(`${e.message} Sending your question anyway.`, 'err');
      setStatus('');
      break;
    case 'delta':
      lv.content += e.content;
      lv.thinking += e.thinking;
      lv.row?.append(e.content, e.thinking);
      break;
    case 'done':
    case 'error': {
      const stat = e.stats;
      const status = e.event === 'done' ? e.status : 'error';
      lv.row?.finish(status, stat);
      const watching = lv.row !== null;
      if (live === lv) live = null;
      paintSend();
      if (!watching && convId !== lv.conversationId) {
        notice(e.event === 'error' ? `The reply in “${lv.title}” failed: ${e.message}` : `Finished replying in “${lv.title}”.`, e.event === 'error' ? 'err' : 'info');
      }
      void sidebar.refresh();
      input.focus();
      break;
    }
  }
}

async function send(): Promise<void> {
  if (live) {
    if (live.conversationId === convId) api.cancel(live.messageId).catch((e) => notice(errMsg(e), 'err'));
    else notice(`A reply is still being generated in “${live.title}”. Open that chat to stop it, or wait for it to finish.`);
    return;
  }
  const typed = input.value.trim();
  if ((!typed && !composer.items.length) || !modelSel.value) {
    if (!modelSel.value) notice('No model selected. Is Ollama running and does it have a model pulled?', 'err');
    return;
  }
  const items: Pending[] = composer.take();
  input.value = '';
  input.style.height = 'auto';
  clearNotice();
  try {
    const lv: Live = { conversationId: 0, messageId: 0, title: '', content: '', thinking: '', sources: null, searchError: null, row: null };
    const ids = await api.send(
      { conversationId: convId, model: modelSel.value, text: typed, attachments: Composer.payload(items), numCtx: Number(ctxSel.value), web: webOn && typed.length > 0 },
      (e) => handleEvent(lv, e),
    );
    Object.assign(lv, { conversationId: ids.conversationId, messageId: ids.assistantMessageId, title: ids.title });
    live = lv;
    if (convId === null) {
      convId = ids.conversationId;
      msgs.replaceChildren();
      convTitle.textContent = ids.title;
      save('last_conversation', String(convId));
    } else {
      msgs.querySelector('.blank')?.remove();
    }
    sidebar.setActive(convId);
    msgs.appendChild(userRow(typed, items.map((a) => ({ kind: a.kind, name: a.name, pages: a.pages, dataUrl: a.dataUrl })), blobs));
    const row = new AssistantRow(ids.assistantMessageId, modelSel.value, scrollHost);
    row.setLive(true);
    lv.row = row;
    msgs.appendChild(row.el);
    log.scrollTop = log.scrollHeight;
    setStatus('');
    paintSend();
    void sidebar.refresh();
  } catch (e) {
    // Nothing was sent or stored: put the user's input back.
    input.value = typed;
    composer.restore(items);
    notice(`Could not send: ${errMsg(e)}`, 'err');
  }
}

// ---------- export ----------

async function exportCurrent(): Promise<void> {
  if (convId === null) return notice('Nothing to export yet.');
  try {
    const { conversation, messages } = await api.load(convId);
    const path = await api.pickSavePath(safeFilename(conversation.title, 'md'), 'Markdown', 'md');
    if (!path) return;
    await api.saveText(path, conversationToMarkdown(conversation, messages));
    notice(`Exported to ${path}`);
  } catch (e) {
    notice(`Export failed: ${errMsg(e)}`, 'err');
  }
}

// ---------- wiring ----------

const sidebar = new Sidebar($('convs'), $<HTMLInputElement>('search'), $('trashBtn'), {
  open: (id) => void openConversation(id),
  deleted: (id) => {
    if (id === convId) newChat();
  },
});

function setSidebar(open: boolean): void {
  document.body.classList.toggle('noside', !open);
}

function wire(): void {
  webBtn.onclick = () => {
    webOn = !webOn;
    save('web', webOn ? '1' : '0');
    paintWeb();
  };
  ctxSel.onchange = () => {
    save('ctx', ctxSel.value);
    composer.render();
  };
  modelSel.onchange = () => {
    save('last_model', modelSel.value);
    void refreshVision();
  };
  $('clear').onclick = newChat;
  $('exportMd').onclick = () => void exportCurrent();
  $('settingsBtn').onclick = () =>
    openSettings({
      getPort: () => S.ollama_port ?? '11434',
      getReduce: () => document.body.classList.contains('reduce'),
      setReduce: (on) => {
        applyReduce(on);
        save('reduce_effects', on ? '1' : '0');
      },
      portChanged: async () => {
        S.ollama_port = (await api.settings()).ollama_port ?? '11434';
        const r = await api.ollamaEnsure();
        if (r.state === 'failed') return { ok: false, message: r.message };
        await loadModels();
        return { ok: true, message: r.state === 'started' ? 'Ollama was not running, so it was started on this port.' : 'Connected to Ollama.' };
      },
    });
  $('sideToggle').onclick = () => {
    const open = document.body.classList.contains('noside');
    setSidebar(open);
    save('sidebar', open ? '1' : '0');
  };
  sendBtn.onclick = () => void send();
  $('attach').onclick = () => $<HTMLInputElement>('file').click();
  const fileIn = $<HTMLInputElement>('file');
  fileIn.onchange = () => {
    void composer.addFiles([...(fileIn.files ?? [])]);
    fileIn.value = '';
  };
  input.addEventListener('paste', (e) => {
    const f = [...(e.clipboardData?.files ?? [])];
    if (f.length) {
      e.preventDefault();
      void composer.addFiles(f);
    }
  });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send();
    }
  });
  input.addEventListener('input', () => {
    input.style.height = 'auto';
    input.style.height = Math.min(input.scrollHeight, 200) + 'px';
  });

  // drag and drop (the window is created with native file-drop handling off, so these are real DOM events)
  let depth = 0;
  const drop = $('drop');
  const hasFiles = (e: DragEvent) => !!e.dataTransfer && [...e.dataTransfer.types].includes('Files');
  addEventListener('dragenter', (e) => {
    if (hasFiles(e)) {
      depth++;
      drop.classList.add('on');
    }
  });
  addEventListener('dragleave', () => {
    if (--depth <= 0) {
      depth = 0;
      drop.classList.remove('on');
    }
  });
  addEventListener('dragover', (e) => e.preventDefault());
  addEventListener('drop', (e) => {
    e.preventDefault();
    depth = 0;
    drop.classList.remove('on');
    if (e.dataTransfer?.files.length) void composer.addFiles([...e.dataTransfer.files]);
  });

  // links never navigate the webview; copy buttons inside code blocks
  document.addEventListener('click', (e) => {
    const t = e.target as Element;
    const a = t.closest('a[href]');
    if (a) {
      e.preventDefault();
      api.openExternal(a.getAttribute('href') ?? '').catch((err) => notice(errMsg(err), 'err'));
      return;
    }
    const cp = t.closest('pre .copy');
    if (cp) void copyWithFeedback(cp as HTMLElement, cp.closest('pre')?.querySelector('code')?.textContent ?? '');
  });
  document.addEventListener('auxclick', (e) => {
    if ((e.target as Element).closest('a[href]')) e.preventDefault();
  });

  document.addEventListener('keydown', (e) => {
    if (!(e.ctrlKey || e.metaKey)) return;
    if (e.key === 'n') {
      e.preventDefault();
      newChat();
    } else if (e.key === 'k') {
      e.preventDefault();
      setSidebar(true);
      $('search').focus();
    }
  });
}

function paintWeb(): void {
  webBtn.classList.toggle('on', webOn);
  webBtn.title = `Search the web before answering (${webOn ? 'on' : 'off'}). Results are used for that one answer only and never saved into the chat context.`;
}

async function boot(): Promise<void> {
  wire();
  showBlank();
  try {
    S = await api.settings();
  } catch (e) {
    notice(`Could not read settings: ${errMsg(e)}`, 'err');
  }
  ctxSel.value = S.ctx && [...ctxSel.options].some((o) => o.value === S.ctx) ? S.ctx : '8192';
  webOn = S.web === '1';
  paintWeb();
  setSidebar(S.sidebar !== '0');
  if (S.reduce_effects !== undefined) applyReduce(S.reduce_effects === '1');
  composer.render();
  await sidebar.refresh();
  const last = Number(S.last_conversation);
  const models = startOllama();
  if (last > 0) {
    try {
      await openConversation(last);
    } catch {
      /* chat may have been deleted; stay on the blank view */
    }
  }
  await models;
  void autoDetectEffects();
  input.focus();
}

void boot();

export type { Conversation };
