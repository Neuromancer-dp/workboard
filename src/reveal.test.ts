import { describe, expect, it } from 'vitest';
import { nextReveal } from './messages';

describe('nextReveal', () => {
  it('always makes progress and never overshoots', () => {
    const t = 'x'.repeat(500);
    let n = 0, frames = 0;
    while (n < t.length) { const m = nextReveal(t, n); expect(m).toBeGreaterThan(n); expect(m).toBeLessThanOrEqual(t.length); n = m; frames++; }
    expect(frames).toBeLessThan(80); // a 500-char burst drains in about a second
  });
  it('types slowly when little is waiting', () => {
    expect(nextReveal('abcdefghij', 8)).toBe(10);
    expect(nextReveal('abcdefghijk', 8)).toBe(10);
  });
  it('does not split surrogate pairs', () => {
    const t = 'ab😀cd';
    expect(nextReveal(t, 1)).toBe(4);
  });
});
