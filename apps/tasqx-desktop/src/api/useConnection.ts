import { createContext, useCallback, useContext, useSyncExternalStore } from 'react';

import type { ApiClient } from './client';
import { ConnectionController, defaultLoadBaseline } from './connection';
import type { ConnectionState } from './connection';
import { TauriTransport } from './transport';

/** The connection the app runs on. A test builds its own on a FakeTransport. */
export function createAppConnection(): ConnectionController {
  return new ConnectionController({
    transport: new TauriTransport(),
    loadBaseline: defaultLoadBaseline,
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
