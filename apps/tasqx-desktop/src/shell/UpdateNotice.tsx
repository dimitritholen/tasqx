import { invoke } from '@tauri-apps/api/core';
import type { MouseEvent } from 'react';

import { isTauri } from '../platform';
import { Button } from '../ui/primitives';
import { APP_VERSION, dismissUpdate, useUpdates } from './updates';

/**
 * "A newer version exists", with the release page and a dismiss (D10, D230).
 * The link is the only action: nothing is downloaded or installed here.
 */
export function UpdateNotice() {
  const { available, dismissed } = useUpdates();
  if (!available || dismissed === available.version) return null;

  // The Tauri window opens no second window of its own, so the host hands the
  // page to the system browser; a dev browser just follows the link.
  const open = (event: MouseEvent<HTMLAnchorElement>) => {
    if (!isTauri()) return;
    event.preventDefault();
    void invoke('open_release_page', { url: available.url });
  };

  return (
    <div className="update-notice" role="status">
      <span className="update-notice-text">
        {`A newer version exists: Tasqx ${available.version} (this is ${APP_VERSION}).`}
      </span>
      <a href={available.url} target="_blank" rel="noreferrer" onClick={open}>
        Release page
      </a>
      <Button size="sm" aria-label={`Dismiss the ${available.version} notice`} onClick={dismissUpdate}>
        Dismiss
      </Button>
    </div>
  );
}
