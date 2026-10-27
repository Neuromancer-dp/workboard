import { describe, expect, it } from 'vitest';
import { MarkdownView } from './markdown';
import { splitThinking } from './think';

function render(text: string, final = true): HTMLElement {
  const el = document.createElement('div');
  new MarkdownView(el).update(text, final);
  return el;
}

describe('markdown rendering', () => {
  it('renders the common block and inline elements', () => {
    const el = render(
      [
        '# Title', '', 'Some **bold**, *italic*, `code` and a [link](https://example.com/a?b=1).', '',
        '- one', '  - nested a', '  - nested b', '- two', '', '1. first', '2. second', '', '> quoted', '', '---', '',
        '| a | b |', '|---|:-:|', '| 1 | 2 |',
      ].join('\n'),
    );
    expect(el.querySelector('h1')?.textContent).toBe('Title');
    expect(el.querySelector('strong')?.textContent).toBe('bold');
    expect(el.querySelector('em')?.textContent).toBe('italic');
    expect(el.querySelector('p code')?.textContent).toBe('code');
    expect(el.querySelector('ul ul li')?.textContent).toBe('nested a');
    expect(el.querySelectorAll('ol > li')).toHaveLength(2);
    expect(el.querySelector('blockquote')?.textContent?.trim()).toBe('quoted');
    expect(el.querySelector('hr')).not.toBeNull();
    const a = el.querySelector('a')!;
    expect(a.getAttribute('href')).toBe('https://example.com/a?b=1');
    expect(a.getAttribute('rel')).toBe('noopener noreferrer');
  });

  it('renders tables inside a scroll wrapper with alignment kept', () => {
    const el = render('| a | b |\n|---|:-:|\n| 1 | 2 |\n| 3 | 4 |');
    expect(el.querySelector('.tablewrap > table')).not.toBeNull();
    expect(el.querySelectorAll('tbody tr')).toHaveLength(2);
    expect(el.querySelectorAll('th')[1].getAttribute('align')).toBe('center');
  });

  it('renders task lists without form controls', () => {
    const el = render('- [x] done\n- [ ] todo');
    expect(el.querySelector('input')).toBeNull();
    expect(el.textContent).toContain('☑ done');
    expect(el.textContent).toContain('☐ todo');
  });

  it('highlights known languages and labels the block', () => {
    const el = render('```python\ndef f(x):\n    return x + 1\n```');
    expect(el.querySelector('.codebar span')?.textContent).toBe('python');
    expect(el.querySelector('.codebar button.copy')).not.toBeNull();
    expect(el.querySelector('pre code .hljs-keyword')?.textContent).toBe('def');
    expect(el.querySelector('pre code')?.textContent).toBe('def f(x):\n    return x + 1\n');
  });

  it('uses aliases and leaves unknown languages as plain text', () => {
    expect(render('```ts\nconst a: number = 1;\n```').querySelector('pre code .hljs-keyword')).not.toBeNull();
    const u = render('```brainfuck\n+++\n```');
    expect(u.querySelector('pre code span')).toBeNull();
    expect(u.querySelector('.codebar span')?.textContent).toBe('brainfuck');
    expect(render('```\nplain\n```').querySelector('.codebar span')?.textContent).toBe('text');
  });

  it('keeps code content escaped', () => {
    const el = render('```html\n<script>alert(1)</script>\n```');
    expect(el.querySelector('script')).toBeNull();
    expect(el.querySelector('pre code')?.textContent).toContain('<script>alert(1)</script>');
  });
});

describe('streaming', () => {
  it('shows an unclosed fence as readable, unhighlighted code, then highlights when it closes', () => {
    const el = document.createElement('div');
    const v = new MarkdownView(el);
    v.update('Here:\n\n```python\ndef f():\n    pass', false);
    expect(el.querySelector('pre code')?.textContent).toBe('def f():\n    pass\n');
    expect(el.querySelector('pre code span')).toBeNull();
    expect(el.querySelector('p')?.textContent).toBe('Here:');
    v.update('Here:\n\n```python\ndef f():\n    pass\n```\n\nDone.', false);
    expect(el.querySelector('pre code .hljs-keyword')).not.toBeNull();
    expect(el.querySelector('pre + p')?.textContent).toBe('Done.');
  });

  it('half-written markup stays readable text', () => {
    for (const partial of ['**bold', 'a [link](http://exa', '| a | b |', '1. one\n2.', '# ', '`code', '> ', '~~~\nx', '<b']) {
      const el = render(partial, false);
      expect(el.textContent!.length).toBeGreaterThan(0);
    }
  });

  it('incremental rendering equals a fresh render at every prefix', () => {
    const doc = [
      '# Heading', '', 'Intro paragraph with **bold** and `code`.', '', '- a', '- b', '  - c', '', '1. x', '2. y', '',
      '```js', 'const x = 1;', 'console.log(x);', '```', '', '```bash', 'ls -la', '```', '', '| h1 | h2 |', '|----|----|', '| a  | b  |', '',
      '> quote line', '> - in quote', '', '1. step', '   1. sub step', '   2. another', '2. next', '',
      '- loose a', '', '- loose b', '', '  continued para', '', 'Setext', '------', '', '    indented code', '', '    more code', '',
      'Final line with a [link](https://example.com).',
    ].join('\n');
    const inc = document.createElement('div');
    const v = new MarkdownView(inc);
    // every prefix with MD_FULL=1 (slow in jsdom); a sampled sweep otherwise
    const env = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env;
    const step = env?.MD_FULL ? 1 : 7;
    for (let n = 1; n <= doc.length; n += step) {
      const prefix = doc.slice(0, n);
      v.update(prefix, false);
      expect(inc.innerHTML, `prefix length ${n}`).toBe(render(prefix, false).innerHTML);
    }
    v.update(doc, true);
    expect(inc.innerHTML).toBe(render(doc, true).innerHTML);
  }, 600_000);

  it('keeps finished blocks as the same DOM nodes while streaming', () => {
    const el = document.createElement('div');
    const v = new MarkdownView(el);
    v.update('First paragraph.\n\nSecond', false);
    const first = el.querySelector('p')!;
    v.update('First paragraph.\n\nSecond paragraph grows', false);
    v.update('First paragraph.\n\nSecond paragraph grows\n\nThird', false);
    expect(el.querySelector('p')).toBe(first);
    expect(el.querySelectorAll('p')).toHaveLength(3);
  });

  it('recovers when the text is replaced rather than appended', () => {
    const el = document.createElement('div');
    const v = new MarkdownView(el);
    v.update('# One\n\nalpha\n\nbeta', false);
    v.update('Totally different', false);
    expect(el.innerHTML.trim()).toBe('<p>Totally different</p>');
  });
});

describe('sanitization', () => {
  const bad = [
    '<script>alert(1)</script>hello',
    '<img src=x onerror=alert(1)>',
    '<svg onload=alert(1)><circle/></svg>',
    '<iframe src="https://evil.example"></iframe>',
    '<style>body{display:none}</style>x',
    '<form action="https://evil.example"><input name=a><button>go</button></form>',
    '<a href="javascript:alert(1)">x</a>',
    '[x](javascript:alert(1))',
    '[x](JaVaScRiPt:alert(1))',
    '[x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)',
    '[x](vbscript:msgbox(1))',
    '<p onclick="alert(1)" style="x:y">t</p>',
    '<object data="x"></object><embed src="x">',
    '<math><mtext><table><mglyph><style><img src=x onerror=alert(1)>',
    '![pixel](https://tracker.example/p.gif)',
    '<a href="https://ok.example" target="_blank" onmouseover="alert(1)">y</a>',
  ];

  it.each(bad)('neutralizes %s', (src) => {
    const el = render(src);
    expect(el.querySelector('script, img, svg, iframe, style, form, input, object, embed, math, link, meta, base')).toBeNull();
    for (const node of Array.from(el.querySelectorAll('*'))) {
      for (const attr of Array.from(node.attributes)) {
        expect(attr.name.startsWith('on'), `${node.tagName} ${attr.name}`).toBe(false);
        expect(['style', 'target', 'src', 'srcset', 'action', 'formaction', 'data']).not.toContain(attr.name);
      }
    }
    for (const a of Array.from(el.querySelectorAll('a[href]'))) {
      expect(a.getAttribute('href')).toMatch(/^(https?:|mailto:)/i);
    }
  });

  it('drops javascript: hrefs but keeps the link text', () => {
    const el = render('[click me](javascript:alert(1))');
    expect(el.querySelector('a[href]')).toBeNull();
    expect(el.textContent).toContain('click me');
  });

  it('keeps safe links and mailto', () => {
    const el = render('[a](https://example.com) [m](mailto:me@example.com)');
    expect(Array.from(el.querySelectorAll('a')).map((a) => a.getAttribute('href'))).toEqual(['https://example.com', 'mailto:me@example.com']);
  });

  it('turns remote images into inert text', () => {
    const el = render('![a cat](https://tracker.example/cat.png)');
    expect(el.querySelector('img')).toBeNull();
    expect(el.textContent).toContain('[image: a cat]');
  });

  it('does not let model output forge UI classes', () => {
    const el = render('<span class="copy codebar live">x</span><div class="sources">y</div>');
    expect(el.querySelector('[class]')).toBeNull();
  });
});

describe('thinking split', () => {
  it('splits inline think blocks', () => {
    expect(splitThinking('<think>plan it</think>\n\nThe answer.')).toEqual({ thinking: 'plan it', answer: 'The answer.', open: false });
  });
  it('treats an unterminated block as still thinking', () => {
    expect(splitThinking('<think>half a thou')).toEqual({ thinking: 'half a thou', answer: '', open: true });
  });
  it('handles templates that only emit the closing tag', () => {
    expect(splitThinking('reasoning here</think>Answer')).toMatchObject({ thinking: 'reasoning here', answer: 'Answer' });
  });
  it('hides partially streamed tags', () => {
    expect(splitThinking('Hello <thi', true).answer).toBe('Hello ');
    expect(splitThinking('<think>abc</thi', true).thinking).toBe('abc');
    expect(splitThinking('1 < 2', true).answer).toBe('1 < 2');
    expect(splitThinking('a <', false).answer).toBe('a <');
  });
  it('handles several blocks', () => {
    const s = splitThinking('a<think>1</think>b<think>2</think>c');
    expect(s).toMatchObject({ thinking: '1\n2', answer: 'abc' });
  });
});
