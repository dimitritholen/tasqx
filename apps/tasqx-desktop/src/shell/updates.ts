import { useEffect, useSyncExternalStore } from 'react';

import { version as APP_VERSION } from '../../package.json';

/**
 * The newer-release notice (D???). D10 allows a passive "a newer version
 * exists" and nothing more: there is no updater, nothing is downloaded or
 * installed, and the notice only links the release page. Asking GitHub is the
 * one network request the app makes on its own, so it is off until the user
 * turns it on (§10: no phone-home), and skipped while the machine is offline.
 */
export { APP_VERSION };

export const UPDATE_CHECK_KEY = 'tasqx.desktop.updateCheck';
export const UPDATE_DISMISSED_KEY = 'tasqx.desktop.updateDismissed';
export const RELEASES_API = 'https://api.github.com/repos/dimitritholen/tasqx/releases/latest';
/** The only pages the notice links, and the only ones the host may open. */
export const RELEASE_PAGES = 'https://github.com/dimitritholen/tasqx/releases/';

export interface Release {
  version: string;
  url: string;
}

interface UpdateState {
  enabled: boolean;
  dismissed: string | null;
  available: Release | null;
}

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // A refused write costs the setting its persistence, nothing else.
  }
}

function fromStorage(): UpdateState {
  return { enabled: read(UPDATE_CHECK_KEY) === 'on', dismissed: read(UPDATE_DISMISSED_KEY), available: null };
}

let state = fromStorage();
const listeners = new Set<() => void>();

function update(patch: Partial<UpdateState>): void {
  state = { ...state, ...patch };
  for (const listener of listeners) listener();
}

/** `1.2.3` or `v1.2.3` as three numbers; a pre-release or anything else is null. */
function parse(version: string): number[] | null {
  const match = /^v?(\d+)\.(\d+)\.(\d+)$/.exec(version.trim());
  return match ? match.slice(1).map(Number) : null;
}

export function isNewer(candidate: string, current: string): boolean {
  const a = parse(candidate);
  const b = parse(current);
  if (!a || !b) return false;
  for (let i = 0; i < 3; i++) {
    if (a[i] !== b[i]) return a[i]! > b[i]!;
  }
  return false;
}

/**
 * Ask GitHub for the latest release, once, if the user turned the check on and
 * the machine is online. Every failure — offline, refused, rate-limited, an
 * odd answer — is no notice: a release check is never worth an error on screen.
 */
export async function checkForUpdate(): Promise<void> {
  if (!state.enabled || navigator.onLine === false) return;
  let release: Release | null = null;
  try {
    const response = await fetch(RELEASES_API, { headers: { Accept: 'application/vnd.github+json' } });
    if (response.ok) {
      const body = (await response.json()) as { tag_name?: unknown; html_url?: unknown };
      const { tag_name: tag, html_url: url } = body;
      if (
        typeof tag === 'string' &&
        typeof url === 'string' &&
        url.startsWith(RELEASE_PAGES) &&
        isNewer(tag, APP_VERSION)
      ) {
        release = { version: tag.replace(/^v/, ''), url };
      }
    }
  } catch {
    // Offline after all, or blocked: no notice.
  }
  update({ available: release });
}

export function setUpdateCheck(on: boolean): void {
  write(UPDATE_CHECK_KEY, on ? 'on' : 'off');
  update({ enabled: on, available: on ? state.available : null });
  if (on) void checkForUpdate();
}

export function dismissUpdate(): void {
  if (!state.available) return;
  write(UPDATE_DISMISSED_KEY, state.available.version);
  update({ dismissed: state.available.version });
}

/** Re-read the stored setting and forget any answer: between tests. */
export function reloadUpdates(): void {
  update(fromStorage());
}

function subscribe(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}

const snapshot = () => state;

export function useUpdates(): UpdateState {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

/** Run the check once at start-up; a no-op while the setting is off. */
export function useUpdateCheckOnStart(): void {
  useEffect(() => {
    void checkForUpdate();
  }, []);
}
