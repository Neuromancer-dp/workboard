import type { Conversation, Message } from './api';
import { splitThinking, mergeThinking } from './think';

function pad(n: number): string {
  return String(n).padStart(2, '0');
}
export function formatDateTime(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function safeFilename(title: string, ext: string): string {
  const base = title.replace(/[\\/:*?"<>|\u0000-\u001f]+/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 60) || 'chat';
  return `${base}.${ext}`;
}

/** A fence long enough to survive backticks inside the text. */
function fence(text: string): string {
  const longest = Math.max(2, ...(text.match(/`+/g) ?? []).map((r) => r.length));
  return '`'.repeat(longest + 1);
}

export function conversationToMarkdown(conv: Conversation, messages: Message[], fmt: (ms: number) => string = formatDateTime): string {
  const out: string[] = [`# ${conv.title}`, '', `_Model: ${conv.model || 'unknown'} · Started ${fmt(conv.createdAt)} · Exported ${fmt(Date.now())}_`, ''];
  for (const m of messages) {
    const who = m.role === 'user' ? 'You' : (m.stats?.model ?? conv.model ?? 'Assistant');
    out.push(`## ${who} — ${fmt(m.createdAt)}`, '');
    for (const a of m.attachments) {
      out.push(a.kind === 'pdf' ? `- 📄 ${a.name} (${a.pages ?? '?'} pages)` : `- 🖼 ${a.name}`);
    }
    if (m.attachments.length) out.push('');
    if (m.role === 'user') {
      if (m.content) out.push(m.content, '');
      continue;
    }
    const s = splitThinking(m.content);
    const thinking = mergeThinking(m.thinking, s.thinking);
    if (thinking) {
      const f = fence(thinking);
      out.push('<details><summary>thinking</summary>', '', f, thinking, f, '', '</details>', '');
    }
    out.push(s.answer || (m.status === 'error' ? '' : ''), '');
    if (m.status === 'error' && m.stats?.error) out.push(`> ⚠ Error: ${m.stats.error}`, '');
    if (m.status === 'stopped') out.push('> _(stopped)_', '');
    if (m.status === 'interrupted') out.push('> _(interrupted before it finished)_', '');
    if (m.sources?.length) {
      out.push('**Sources**', '');
      m.sources.forEach((src, i) => out.push(`${i + 1}. [${src.title}](${src.url})`));
      out.push('');
    }
    if (m.stats?.tokS) out.push(`_${m.stats.tokS.toFixed(1)} tok/s · ${m.stats.evalCount ?? '?'} tokens out_`, '');
  }
  return out.join('\n').replace(/\n{3,}/g, '\n\n').trimEnd() + '\n';
}
