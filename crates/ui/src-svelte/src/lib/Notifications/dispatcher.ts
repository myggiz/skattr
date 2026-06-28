import type { ConfigSnapshot, NotificationMode, PublicKey } from '$lib/ipc/types';
import { pubkeyEq } from '$lib/pubkey';

export interface FocusInputs {
  windowFocused: boolean;
  activeContactId: string | null;
}

export interface ContactInputs {
  id: string;
  nickname: string | null;
  muted: boolean;
}

export interface MessageInputs {
  contact: string;       // contact id (hex pubkey)
  preview: string;       // already-truncated body or empty
}

export function shouldNotify(
  msg: MessageInputs,
  focus: FocusInputs,
  config: ConfigSnapshot,
  contact: ContactInputs,
): boolean {
  if (config.notification_mode === 'off') return false;
  if (contact.muted) return false;
  if (
    focus.windowFocused &&
    pubkeyEq(
      focus.activeContactId as unknown as PublicKey,
      msg.contact as unknown as PublicKey,
    )
  )
    return false;
  return true;
}

export function buildNotification(
  msg: MessageInputs,
  config: ConfigSnapshot,
  contact: ContactInputs,
): { title: string; body: string } {
  const sender = contact.nickname ?? '(unknown)';
  switch (config.notification_mode) {
    case 'full':
      return { title: sender, body: msg.preview || '(empty)' };
    case 'minimal':
      return { title: sender, body: '' };
    case 'generic':
      return { title: 'Skattr', body: 'New message' };
    case 'off':
      return { title: '', body: '' };
  }
}
