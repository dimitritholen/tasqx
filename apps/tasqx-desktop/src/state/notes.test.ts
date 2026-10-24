import { DEFAULT_NOTE_FILTERS, noteHeadline, noteListParams, NOTES_PAGE, splitLinks } from './notes';

describe('noteListParams', () => {
  it('asks for one page of open notes by default, with no query and no links filter', () => {
    expect(noteListParams(DEFAULT_NOTE_FILTERS)).toEqual({ status: 'open', limit: NOTES_PAGE });
  });

  it('sends links only when asked, and the trimmed query only when there is one', () => {
    expect(noteListParams({ query: '  rust  ', links: true, status: 'all' })).toEqual({
      status: 'all',
      limit: NOTES_PAGE,
      links: true,
      query: 'rust',
    });
    expect(noteListParams({ query: '   ', links: false, status: 'archived' })).toEqual({
      status: 'archived',
      limit: NOTES_PAGE,
    });
  });
});

describe('noteHeadline', () => {
  it('is the first non-blank line, trimmed', () => {
    expect(noteHeadline('\n\n  read this later  \nsecond line')).toBe('read this later');
  });

  it('is empty for a blank body', () => {
    expect(noteHeadline(' \n\t')).toBe('');
  });
});

describe('splitLinks', () => {
  it('keeps a body without a link as one plain segment', () => {
    expect(splitLinks('just words')).toEqual([{ text: 'just words' }]);
  });

  it('marks every http and https link, case-insensitively, between the plain text', () => {
    expect(splitLinks('see https://a.example/x and HTTP://b.example then')).toEqual([
      { text: 'see ' },
      { text: 'https://a.example/x', href: 'https://a.example/x' },
      { text: ' and ' },
      { text: 'HTTP://b.example', href: 'HTTP://b.example' },
      { text: ' then' },
    ]);
  });

  it('leaves trailing sentence punctuation and a closing bracket out of the link', () => {
    expect(splitLinks('(see https://a.example/x).')).toEqual([
      { text: '(see ' },
      { text: 'https://a.example/x', href: 'https://a.example/x' },
      { text: ').' },
    ]);
    expect(splitLinks('https://a.example/path, next')).toEqual([
      { text: 'https://a.example/path', href: 'https://a.example/path' },
      { text: ', next' },
    ]);
  });

  it('keeps a query string and fragment inside the link', () => {
    expect(splitLinks('https://a.example/p?q=1&r=2#top')).toEqual([
      { text: 'https://a.example/p?q=1&r=2#top', href: 'https://a.example/p?q=1&r=2#top' },
    ]);
  });

  it('does not link other schemes or a bare scheme', () => {
    expect(splitLinks('javascript:alert(1) file:///etc/passwd https://')).toEqual([
      { text: 'javascript:alert(1) file:///etc/passwd https://' },
    ]);
  });
});
