import { api, errMsg, type Conversation, type SearchHit } from './api';
import { groupConversations, trashDaysLeft } from './timegroup';
import { button, confirmDialog, h, notice } from './ui';

export interface SidebarHooks {
  open(id: number): void;
  /** a chat moved to the trash */
  deleted(id: number): void;
}

/** Marked-up snippet (\u0002 hit \u0003) to DOM, without ever touching innerHTML. */
function snippetEl(s: string): HTMLElement {
  const el = h('span', 'sub');
  const parts = s.replace(/\s+/g, ' ').split(/[\u0002\u0003]/);
  parts.forEach((p, i) => {
    if (!p) return;
    if (i % 2 === 1) el.appendChild(h('mark', '', p));
    else el.appendChild(document.createTextNode(p));
  });
  return el;
}

export class Sidebar {
  activeId: number | null = null;
  generatingId: number | null = null;
  private mode: 'chats' | 'trash' = 'chats';
  private query = '';
  private timer = 0;
  private seq = 0;

  constructor(
    private readonly list: HTMLElement,
    search: HTMLInputElement,
    private readonly trashBtn: HTMLElement,
    private readonly hooks: SidebarHooks,
  ) {
    search.addEventListener('input', () => {
      clearTimeout(this.timer);
      this.timer = window.setTimeout(() => {
        this.query = search.value.trim();
        void this.refresh();
      }, 180);
    });
    search.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        search.value = '';
        this.query = '';
        void this.refresh();
      }
    });
    trashBtn.addEventListener('click', () => {
      this.mode = this.mode === 'trash' ? 'chats' : 'trash';
      void this.refresh();
    });
  }

  setActive(id: number | null): void {
    this.activeId = id;
    this.list.querySelectorAll<HTMLElement>('.citem').forEach((el) => el.classList.toggle('active', Number(el.dataset.id) === id));
  }

  async refresh(): Promise<void> {
    const seq = ++this.seq;
    this.trashBtn.classList.toggle('on', this.mode === 'trash');
    this.trashBtn.textContent = this.mode === 'trash' ? 'Back to chats' : 'Trash';
    try {
      let nodes: Node[];
      if (this.mode === 'trash') nodes = this.trashView(await api.trash());
      else if (this.query) nodes = this.searchView(await api.searchConversations(this.query));
      else nodes = this.chatsView(await api.conversations());
      if (seq !== this.seq) return; // a newer refresh won
      this.list.replaceChildren(...nodes);
    } catch (e) {
      notice(`Could not load the chat list: ${errMsg(e)}`, 'err');
    }
  }

  private chatsView(convs: Conversation[]): Node[] {
    if (!convs.length) return [h('div', 'empty-note', 'No chats yet. Your conversations are saved here automatically.')];
    const out: Node[] = [];
    for (const g of groupConversations(convs)) {
      out.push(h('div', 'grp', g.label));
      for (const c of g.items) out.push(this.item(c));
    }
    return out;
  }

  private searchView(hits: SearchHit[]): Node[] {
    if (!hits.length) return [h('div', 'empty-note', 'No matches.')];
    return hits.map((c) => this.item(c, c.snippet));
  }

  private trashView(convs: Conversation[]): Node[] {
    const bar = h('div', 'trash-bar');
    bar.append(h('span', '', 'Kept for 30 days, then removed.'), h('span', 'sp'));
    const empty = button('Empty', async () => {
      const ok = await confirmDialog({
        title: 'Empty trash',
        message: `Permanently delete ${convs.length} chat${convs.length === 1 ? '' : 's'}? This cannot be undone.`,
        confirm: 'Delete forever',
        danger: true,
      });
      if (!ok) return;
      try {
        await api.emptyTrash();
      } catch (e) {
        notice(errMsg(e), 'err');
      }
      void this.refresh();
    });
    empty.disabled = convs.length === 0;
    bar.appendChild(empty);
    if (!convs.length) return [bar, h('div', 'empty-note', 'Trash is empty.')];
    return [bar, ...convs.map((c) => this.item(c))];
  }

  private item(c: Conversation, snippet = ''): HTMLElement {
    const el = h('div', 'citem');
    el.dataset.id = String(c.id);
    el.tabIndex = 0;
    el.setAttribute('role', 'button');
    el.classList.toggle('active', c.id === this.activeId && !c.deletedAt);
    const t = h('span', 't');
    if (c.pinned) t.appendChild(h('span', 'pin', '★'));
    if (c.id === this.generatingId) t.appendChild(h('span', 'pin', '●'));
    t.appendChild(document.createTextNode(c.title));
    el.appendChild(t);
    if (snippet) el.appendChild(snippetEl(snippet));
    else if (c.deletedAt) el.appendChild(h('span', 'sub', `${trashDaysLeft(c.deletedAt)} days left`));
    else el.appendChild(h('span', 'sub', c.model));

    const acts = h('span', 'acts');
    const stop = (fn: () => void) => (e: MouseEvent) => {
      e.stopPropagation();
      fn();
    };
    if (c.deletedAt) {
      acts.append(
        button('↺', stop(() => this.act(() => api.restore(c.id))), '', 'Restore'),
        button('✕', stop(() => this.purge(c)), '', 'Delete forever'),
      );
    } else {
      acts.append(
        button(c.pinned ? '☆' : '★', stop(() => this.act(() => api.pin(c.id, !c.pinned))), '', c.pinned ? 'Unpin' : 'Pin to top'),
        button('✎', stop(() => this.rename(el, t, c)), '', 'Rename'),
        button('✕', stop(() => this.trash(c)), '', 'Move to Trash'),
      );
    }
    el.appendChild(acts);
    if (!c.deletedAt) {
      el.addEventListener('click', () => this.hooks.open(c.id));
      el.addEventListener('keydown', (e) => {
        if (e.target === el && (e.key === 'Enter' || e.key === ' ')) {
          e.preventDefault();
          this.hooks.open(c.id);
        }
      });
    }
    return el;
  }

  private async act(fn: () => Promise<unknown>): Promise<void> {
    try {
      await fn();
    } catch (e) {
      notice(errMsg(e), 'err');
    }
    void this.refresh();
  }

  private rename(el: HTMLElement, t: HTMLElement, c: Conversation): void {
    const input = h('input', 'rename');
    input.value = c.title;
    input.maxLength = 200;
    let done = false;
    const finish = (save: boolean) => {
      if (done) return;
      done = true;
      const v = input.value.trim();
      if (save && v && v !== c.title) void this.act(() => api.rename(c.id, v));
      else void this.refresh();
    };
    input.addEventListener('keydown', (e) => {
      e.stopPropagation();
      if (e.key === 'Enter') finish(true);
      else if (e.key === 'Escape') finish(false);
    });
    input.addEventListener('blur', () => finish(true));
    input.addEventListener('click', (e) => e.stopPropagation());
    t.replaceWith(input);
    el.querySelector('.acts')?.remove();
    input.focus();
    input.select();
  }

  private async trash(c: Conversation): Promise<void> {
    const ok = await confirmDialog({
      title: 'Move to Trash',
      message: `Move “${c.title}” to the Trash?\n\nYou can restore it from the Trash for 30 days.`,
      confirm: 'Move to Trash',
      danger: true,
    });
    if (!ok) return;
    try {
      await api.remove(c.id);
      this.hooks.deleted(c.id);
    } catch (e) {
      notice(errMsg(e), 'err');
    }
    void this.refresh();
  }

  private async purge(c: Conversation): Promise<void> {
    const ok = await confirmDialog({
      title: 'Delete forever',
      message: `Permanently delete “${c.title}” and its attachments? This cannot be undone.`,
      confirm: 'Delete forever',
      danger: true,
    });
    if (ok) await this.act(() => api.purge(c.id));
  }
}
