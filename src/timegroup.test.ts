import { describe, expect, it } from 'vitest';
import { groupConversations, trashDaysLeft } from './timegroup';

const now = new Date(2026, 9, 27, 15, 30); // Tue 27 Oct 2026, local
const at = (y: number, m: number, d: number, h = 12) => new Date(y, m, d, h).getTime();
const c = (id: number, updatedAt: number, pinned = false) => ({ id, updatedAt, pinned });

describe('groupConversations', () => {
  it('buckets by local calendar day and puts pinned first', () => {
    const list = [
      c(1, at(2026, 9, 27, 9)),
      c(2, at(2026, 9, 26, 23)),
      c(3, at(2026, 9, 21, 8)),
      c(4, at(2026, 9, 19, 23)),
      c(5, at(2026, 0, 1)),
      c(6, at(2026, 8, 1), true),
    ];
    const g = groupConversations(list, now);
    expect(g.map((x) => [x.label, x.items.map((i) => i.id)])).toEqual([
      ['Pinned', [6]],
      ['Today', [1]],
      ['Yesterday', [2]],
      ['Previous 7 days', [3]], // Oct 21; Oct 20 would still count, Oct 19 does not
      ['Older', [4, 5]],
    ]);
  });

  it('midnight boundaries', () => {
    const g = groupConversations([c(1, at(2026, 9, 27, 0)), c(2, new Date(2026, 9, 26, 23, 59, 59).getTime())], now);
    expect(g.map((x) => x.label)).toEqual(['Today', 'Yesterday']);
  });

  it('omits empty groups', () => {
    expect(groupConversations([], now)).toEqual([]);
  });
});

describe('trashDaysLeft', () => {
  it('counts down from 30 and never goes negative', () => {
    const t = Date.UTC(2026, 9, 1);
    expect(trashDaysLeft(t, t)).toBe(30);
    expect(trashDaysLeft(t, t + 86_400_000 * 10 + 5)).toBe(20);
    expect(trashDaysLeft(t, t + 86_400_000 * 40)).toBe(0);
  });
});
