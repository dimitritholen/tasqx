import { frameSse, splitLines } from './daemonBridge';

// The framing helpers only: a real Vite dev/preview server is not spun up in
// tests, see src/test/integration/daemon.test.ts for the one test that opens
// a real socket.
describe('daemon bridge framing', () => {
  it('frames a daemon line as an SSE message', () => {
    expect(frameSse('{"ok":true}')).toBe('data: {"ok":true}\n\n');
  });

  it('splits complete lines off a chunk and holds the remainder', () => {
    expect(splitLines('{"a":1}\n{"b":2}\n{"c"')).toEqual({
      lines: ['{"a":1}', '{"b":2}'],
      rest: '{"c"',
    });
  });

  it('drops empty lines and keeps an empty remainder when the chunk ends cleanly', () => {
    expect(splitLines('one\n\ntwo\n')).toEqual({ lines: ['one', 'two'], rest: '' });
  });
});
