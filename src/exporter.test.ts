import { describe, expect, it } from 'vitest';
import type { Conversation, Message } from './api';
import { conversationToMarkdown, safeFilename } from './exporter';

const conv: Conversation = { id: 1, title: 'Hyprland help', model: 'llama3', createdAt: 1, updatedAt: 2, pinned: false, deletedAt: null };
const base = { conversationId: 1, createdAt: 1000, thinking: '', status: 'complete' as const, stats: null, sources: null, attachments: [] };
const fmt = () => 'T';

describe('conversationToMarkdown', () => {
  it('exports turns, thinking, sources and attachments', () => {
    const msgs: Message[] = [
      { ...base, id: 1, role: 'user', content: 'what is [1]?', attachments: [{ id: 1, kind: 'pdf', name: 'a.pdf', mime: 'application/pdf', pages: 3, chars: 10, truncated: false }] },
      {
        ...base, id: 2, role: 'assistant', content: '<think>hmm</think>It is **x** [1].', thinking: 'field thought',
        sources: [{ title: 'Doc', url: 'https://e.example' }], stats: { model: 'llama3', tokS: 12.34, evalCount: 50 },
      },
    ];
    const md = conversationToMarkdown(conv, msgs, fmt);
    expect(md).toContain('# Hyprland help');
    expect(md).toContain('## You — T');
    expect(md).toContain('- 📄 a.pdf (3 pages)');
    expect(md).toContain('## llama3 — T');
    expect(md).toContain('<summary>thinking</summary>');
    expect(md).toMatch(/field thought\n\nhmm/);
    expect(md).toContain('It is **x** [1].');
    expect(md).not.toContain('<think>');
    expect(md).toContain('1. [Doc](https://e.example)');
    expect(md).toContain('12.3 tok/s · 50 tokens out');
    expect(md.endsWith('\n')).toBe(true);
  });

  it('notes errors and interrupted replies without dropping the text', () => {
    const msgs: Message[] = [
      { ...base, id: 2, role: 'assistant', content: 'partial', status: 'error', stats: { error: 'boom' } },
      { ...base, id: 3, role: 'assistant', content: 'cut', status: 'interrupted' },
    ];
    const md = conversationToMarkdown(conv, msgs, fmt);
    expect(md).toContain('partial');
    expect(md).toContain('Error: boom');
    expect(md).toContain('interrupted');
  });

  it('uses a longer fence when thinking contains backticks', () => {
    const md = conversationToMarkdown(conv, [{ ...base, id: 2, role: 'assistant', content: 'x', thinking: 'a ``` b' }], fmt);
    expect(md).toContain('````\na ``` b\n````');
  });
});

describe('safeFilename', () => {
  it('strips path characters', () => {
    expect(safeFilename('a/b:c*?"<>|d', 'md')).toBe('a b c d.md');
    expect(safeFilename('   ', 'json')).toBe('chat.json');
    expect(safeFilename('x'.repeat(100), 'md')).toHaveLength(63);
  });
});
