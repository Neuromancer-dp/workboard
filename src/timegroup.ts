import type { Conversation } from './api';

export interface Group<T> {
  label: string;
  items: T[];
}

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** Pinned first, then Today / Yesterday / Previous 7 days / Older by last activity. Input order is kept inside a group. */
export function groupConversations<T extends Pick<Conversation, 'pinned' | 'updatedAt'>>(list: T[], now: Date = new Date()): Group<T>[] {
  const today = startOfDay(now);
  const yesterday = startOfDay(new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1));
  const week = startOfDay(new Date(now.getFullYear(), now.getMonth(), now.getDate() - 7));
  const groups: Group<T>[] = [
    { label: 'Pinned', items: [] },
    { label: 'Today', items: [] },
    { label: 'Yesterday', items: [] },
    { label: 'Previous 7 days', items: [] },
    { label: 'Older', items: [] },
  ];
  for (const c of list) {
    const g = c.pinned ? 0 : c.updatedAt >= today ? 1 : c.updatedAt >= yesterday ? 2 : c.updatedAt >= week ? 3 : 4;
    groups[g].items.push(c);
  }
  return groups.filter((g) => g.items.length);
}

/** Whole days left before a trashed chat is removed for good. */
export function trashDaysLeft(deletedAt: number, now = Date.now(), keepDays = 30): number {
  return Math.max(0, Math.ceil((deletedAt + keepDays * 86_400_000 - now) / 86_400_000));
}
