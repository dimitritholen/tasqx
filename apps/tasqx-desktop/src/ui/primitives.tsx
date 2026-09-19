import { cloneElement, useId } from 'react';
import type { ComponentPropsWithoutRef, ReactElement, ReactNode } from 'react';

import { isMac } from '../platform';
import { Icon } from './icons';
import type { IconName } from './icons';

/** Join class names, dropping the falsy ones. */
export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(' ');
}

type ButtonBase = ComponentPropsWithoutRef<'button'> & {
  variant?: 'primary' | 'secondary' | 'ghost' | 'danger';
  size?: 'sm' | 'md';
  loading?: boolean;
};

/** Labelled button. An icon-only control is an IconButton, which forces a name. */
export function Button({
  variant = 'secondary',
  size = 'md',
  loading = false,
  disabled,
  className,
  children,
  ...rest
}: ButtonBase & { children: ReactNode }) {
  return (
    <button
      type="button"
      className={cx('btn', `btn-${variant}`, `btn-${size}`, className)}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading && <Spinner />}
      {children}
    </button>
  );
}

export function IconButton({
  icon,
  label,
  className,
  ...rest
}: Omit<ButtonBase, 'children' | 'variant' | 'size'> & { icon: IconName; label: string }) {
  return (
    <button type="button" className={cx('icon-btn', className)} aria-label={label} title={label} {...rest}>
      <Icon name={icon} />
    </button>
  );
}

export type Status = 'pending' | 'active' | 'done' | 'blocked' | 'overdue' | 'backlog' | 'waiting' | 'warning';

export function Pill({ status, title, children }: { status: Status; title?: string; children: ReactNode }) {
  return (
    <span className={cx('pill', `pill-${status}`)} title={title}>
      {children}
    </span>
  );
}

export function Panel({
  title,
  actions,
  className,
  children,
}: {
  title: string;
  actions?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  const id = useId();
  return (
    <section className={cx('panel', className)} aria-labelledby={id}>
      <header className="panel-header">
        <h2 className="panel-title" id={id}>
          {title}
        </h2>
        {actions && <div className="panel-actions">{actions}</div>}
      </header>
      <div className="panel-body">{children}</div>
    </section>
  );
}

export function EmptyState({ title, message, action }: { title: string; message: ReactNode; action?: ReactNode }) {
  return (
    <div className="empty-state" role="status">
      <h3 className="empty-state-title">{title}</h3>
      <p className="empty-state-text">{message}</p>
      {action}
    </div>
  );
}

/**
 * A slice that failed, in the daemon's own words: the code in mono, the
 * message verbatim, and the one thing left to do about it. Typed structurally
 * rather than as an ApiError so the UI layer keeps no dependency on the API.
 */
export function ErrorState({
  title,
  error,
  onRetry,
}: {
  title: string;
  error: { code: string; message: string };
  onRetry: () => void;
}) {
  return (
    <EmptyState
      title={title}
      message={
        <>
          <span className="mono">{error.code}</span> {error.message}
        </>
      }
      action={<Button onClick={onRetry}>Retry</Button>}
    />
  );
}

/** A placeholder block while a read is in flight; it says nothing, so it is hidden. */
export function Skeleton({ width }: { width?: string }) {
  return <span className="skeleton" style={width === undefined ? undefined : { width }} aria-hidden="true" />;
}

export function Spinner() {
  return <span className="spinner" role="status" aria-label="Loading" />;
}

type Describable = { id?: string; 'aria-describedby'?: string; 'aria-invalid'?: boolean };

/** Label, control and messages. The control is the single child; Field wires the ids. */
export function Field({
  label,
  hint,
  error,
  children,
}: {
  label: string;
  hint?: string;
  error?: string;
  children: ReactElement<Describable>;
}) {
  const id = useId();
  const hintId = hint ? `${id}-hint` : undefined;
  const errorId = error ? `${id}-error` : undefined;
  const describedBy = cx(hintId, errorId) || undefined;
  return (
    <div className="field">
      <label className="field-label" htmlFor={id}>
        {label}
      </label>
      {cloneElement(children, { id, 'aria-describedby': describedBy, 'aria-invalid': error ? true : undefined })}
      {hint && (
        <span className="field-hint" id={hintId}>
          {hint}
        </span>
      )}
      {error && (
        <span className="field-error" id={errorId}>
          {error}
        </span>
      )}
    </div>
  );
}

/** Shortcut display: "mod+k" → ⌘K on macOS, Ctrl+K elsewhere; "g d" → two keys. */
export function Kbd({ keys }: { keys: string }) {
  const mac = isMac();
  return (
    <span className="kbd-group">
      {/* `g g` is two chords of the same key, so the position is the identity. */}
      {keys.split(' ').map((chord, at) => (
        <kbd className="kbd" key={`${chord}-${at}`}>
          {chord
            .split('+')
            .map((part) => (part === 'mod' ? (mac ? '⌘' : 'Ctrl') : part.length === 1 ? part.toUpperCase() : part))
            .join(mac ? '' : '+')}
        </kbd>
      ))}
    </span>
  );
}
