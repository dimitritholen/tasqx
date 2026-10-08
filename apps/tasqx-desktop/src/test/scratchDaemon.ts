import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
// Imported rather than taken off the globals: `types` in tsconfig.json
// deliberately does not carry "node", so app code cannot reach them.
import { env as processEnv, platform } from 'node:process';
import { fileURLToPath } from 'node:url';

import { NodeSocketTransport } from './nodeTransport';

/**
 * A real `tasqx daemon` on a store in a temp directory that nothing else can
 * see, for the tests that must talk to the real thing. Tests only.
 *
 * Every child it starts carries TASQX_DB and TASQX_SOCK pointing into the temp
 * directory, so neither the daemon nor a CLI call can reach the developer's
 * own store or the default socket.
 */

const HERE = fileURLToPath(import.meta.url);
export const REPO_ROOT = resolve(HERE, '../../../../..');
export const BIN = join(REPO_ROOT, 'target', 'debug', platform === 'win32' ? 'tasqx.exe' : 'tasqx');

export async function until(what: string, ready: () => boolean, budgetMs: number): Promise<void> {
  const deadline = Date.now() + budgetMs;
  while (!ready()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((done) => setTimeout(done, 50));
  }
}

export function ensureBinary(): void {
  if (!existsSync(BIN)) {
    execFileSync('cargo', ['build', '-p', 'tasqx-cli'], { cwd: REPO_ROOT, stdio: 'inherit' });
  }
  if (!existsSync(BIN)) throw new Error(`no tasqx binary at ${BIN} — build tasqx-cli first`);
}

export class ScratchDaemon {
  readonly scratch: string;
  readonly db: string;
  /** What the daemon is told to bind: a socket path, or a Windows pipe NAME. */
  readonly socketArg: string;
  /** What `net.createConnection` is given, which on Windows is the pipe's path. */
  readonly socketAddress: string;
  readonly env: Record<string, string | undefined>;
  private child: ChildProcess | null = null;

  constructor() {
    ensureBinary();
    this.scratch = mkdtempSync(join(tmpdir(), 'tasqx-desktop-'));
    this.db = join(this.scratch, 'tasks.db');
    // Windows has no socket file: the daemon takes a bare pipe name (anything
    // path-shaped gets sanitized and hashed, so the test could not name it) and
    // Node connects to it under \\.\pipe\.
    const pipe = `tasqx-desktop-${randomUUID().slice(0, 8)}`;
    this.socketArg = platform === 'win32' ? pipe : join(this.scratch, 'd.sock');
    this.socketAddress = platform === 'win32' ? `\\\\.\\pipe\\${pipe}` : this.socketArg;
    this.env = { ...processEnv, TASQX_DB: this.db, TASQX_SOCK: this.socketArg };
  }

  /** A one-shot CLI call, in-process against the scratch store — never the real one. */
  cli(args: string[], options: { db?: string; input?: string } = {}): string {
    const env = options.db === undefined ? this.env : { ...this.env, TASQX_DB: options.db };
    return execFileSync(BIN, ['--no-daemon', ...args], { env, encoding: 'utf8', input: options.input });
  }

  get running(): boolean {
    return this.child !== null && this.child.exitCode === null && this.child.signalCode === null;
  }

  async start(): Promise<void> {
    this.child = spawn(BIN, ['--socket', this.socketArg, 'daemon', '--db', this.db], {
      env: this.env,
      stdio: ['ignore', 'ignore', 'pipe'],
    });
    await this.waitForListener(20_000);
  }

  /** Stop THIS daemon, by its own process handle; nothing is killed by name. */
  async stop(): Promise<void> {
    const child = this.child;
    if (child === null || !this.running) return;
    const exited = once(child, 'exit');
    child.kill('SIGTERM');
    // Windows still holds tasks.db open until the process is actually gone, so
    // whatever comes next has to wait for it — but bounded, so a daemon that
    // ignores SIGTERM can never hang teardown.
    const timedOut = await Promise.race([
      exited.then(() => false),
      new Promise<boolean>((done) => setTimeout(() => done(true), 5_000)),
    ]);
    if (timedOut) {
      child.kill('SIGKILL');
      await exited;
    }
  }

  async dispose(): Promise<void> {
    await this.stop();
    // maxRetries/retryDelay: the OS can lag a beat behind the exit event in
    // releasing its handle on tasks.db.
    rmSync(this.scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }

  /** Up means a connection actually completes — a socket file can exist unbound. */
  private async waitForListener(budgetMs: number): Promise<void> {
    const deadline = Date.now() + budgetMs;
    for (;;) {
      const probe = new NodeSocketTransport(this.socketAddress);
      try {
        await probe.connect();
        await probe.close();
        return;
      } catch {
        // Not accepting yet.
      }
      if (Date.now() > deadline) {
        throw new Error(
          `daemon never listened on ${this.socketAddress} (exited: ${this.child?.exitCode ?? 'no'})`,
        );
      }
      await new Promise((done) => setTimeout(done, 100));
    }
  }
}
