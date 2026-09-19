import { createContext, useCallback, useContext, useSyncExternalStore } from 'react';

import { isTauri } from '../platform';
import type { DashboardStore } from '../state/store';
import { loadBaseline } from './baseline';
import type { ApiClient } from './client';
import { ConnectionController } from './connection';
import type { ConnectionState } from './connection';
import { DevHttpTransport } from './devTransport';
import { TauriTransport } from './transport';

/**
 * The connection the app runs on: TauriTransport inside the Tauri window,
 * DevHttpTransport (vite-plugins/daemonBridge.ts) everywhere else — a plain
 * browser has no host commands, only whatever the dev/preview server bridges
 * in. Loads #691's baseline into `store`. A test builds its own on a
 * FakeTransport.
 */
export function createAppConnection(store: DashboardStore): ConnectionController {
  return new ConnectionController({
    transport: isTauri() ? new TauriTransport() : new DevHttpTransport(),
    loadBaseline: (client) => loadBaseline(client, store),
  });
}

export const ConnectionContext = createContext<ConnectionController | null>(null);

/** The controller's state as a React store, plus the controller and its client. */
export function useConnection(): {
  state: ConnectionState;
  controller: ConnectionController;
  client: ApiClient;
} {
  const controller = useContext(ConnectionContext);
  if (controller === null) throw new Error('useConnection needs a ConnectionContext provider');
  const subscribe = useCallback((onChange: () => void) => controller.subscribe(onChange), [controller]);
  const getState = useCallback(() => controller.getState(), [controller]);
  const state = useSyncExternalStore(subscribe, getState, getState);
  return { state, controller, client: controller.client };
}

/**
 * Refresh means "make what I am looking at true again": a fresh baseline while
 * live, and an early retry while it is not. The toolbar button, the `r` key,
 * the palette and every Retry in an error state run this one thing.
 */
export function useRefresh(): () => void {
  const { state, controller } = useConnection();
  return useCallback(() => {
    if (state.status === 'live') controller.resync('manual refresh');
    else void controller.retryNow();
  }, [controller, state.status]);
}
