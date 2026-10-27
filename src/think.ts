/** Separate <think>…</think> reasoning from the answer. Works on half-streamed text. */
export interface Split {
  thinking: string;
  answer: string;
  /** true while a <think> block is still open (the model is mid-thought) */
  open: boolean;
}

const OPEN = '<think>';
const CLOSE = '</think>';
// A tag that is only partly streamed must not flash on screen.
const PARTIAL_TAG = /<\/?(t(h(i(n(k)?)?)?)?)?$/;

export function splitThinking(content: string, streaming = false): Split {
  let thinking = '';
  let answer = '';
  let rest = content;
  let open = false;

  // Some chat templates open the block themselves, so only </think> shows up in the output.
  const firstOpen = rest.indexOf(OPEN);
  const firstClose = rest.indexOf(CLOSE);
  if (firstClose !== -1 && (firstOpen === -1 || firstClose < firstOpen)) {
    thinking = rest.slice(0, firstClose);
    rest = rest.slice(firstClose + CLOSE.length);
  }

  for (;;) {
    const i = rest.indexOf(OPEN);
    if (i === -1) {
      answer += rest;
      break;
    }
    answer += rest.slice(0, i);
    const j = rest.indexOf(CLOSE, i + OPEN.length);
    if (j === -1) {
      thinking += (thinking ? '\n' : '') + rest.slice(i + OPEN.length);
      open = true;
      break;
    }
    thinking += (thinking ? '\n' : '') + rest.slice(i + OPEN.length, j);
    rest = rest.slice(j + CLOSE.length);
  }

  if (streaming) {
    if (open) thinking = thinking.replace(PARTIAL_TAG, '');
    else answer = answer.replace(PARTIAL_TAG, '');
  }
  return { thinking: thinking.trim(), answer: answer.replace(/^\s+/, ''), open };
}

/** Combine the separate "thinking" field some models stream with any inline <think> block. */
export function mergeThinking(field: string, inline: string): string {
  return [field.trim(), inline].filter(Boolean).join('\n\n');
}
