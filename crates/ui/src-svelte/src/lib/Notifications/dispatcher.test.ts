import { describe, it, expect } from 'vitest';
import { shouldNotify, buildNotification } from './dispatcher';
import type { ConfigSnapshot } from '$lib/ipc/types';

const baseCfg = (mode: ConfigSnapshot['notification_mode']): ConfigSnapshot => ({
  history_retention_days: 0,
  direct_timeout_secs: 30,
  notification_mode: mode,
  close_to_tray: true,
  start_minimised: false,
  persist_logs_to_disk: false,
});

describe('shouldNotify truth table', () => {
  const msg = { contact: 'alice', preview: 'hi' };
  const contactNotMuted = { id: 'alice', nickname: 'Alice', muted: false };
  const contactMuted = { id: 'alice', nickname: 'Alice', muted: true };

  it('off mode never notifies', () => {
    expect(
      shouldNotify(msg, { windowFocused: false, activeContactId: null }, baseCfg('off'), contactNotMuted),
    ).toBe(false);
  });

  it('muted contact never notifies', () => {
    expect(
      shouldNotify(msg, { windowFocused: false, activeContactId: null }, baseCfg('full'), contactMuted),
    ).toBe(false);
  });

  it('focused + active conversation suppresses', () => {
    expect(
      shouldNotify(msg, { windowFocused: true, activeContactId: 'alice' }, baseCfg('full'), contactNotMuted),
    ).toBe(false);
  });

  it('focused + DIFFERENT conversation still notifies', () => {
    expect(
      shouldNotify(msg, { windowFocused: true, activeContactId: 'bob' }, baseCfg('full'), contactNotMuted),
    ).toBe(true);
  });

  it('unfocused notifies', () => {
    expect(
      shouldNotify(msg, { windowFocused: false, activeContactId: 'alice' }, baseCfg('full'), contactNotMuted),
    ).toBe(true);
  });
});

describe('buildNotification', () => {
  const msg = { contact: 'alice', preview: 'hi there' };
  const c = { id: 'alice', nickname: 'Alice', muted: false };

  it('full → sender + preview', () => {
    expect(buildNotification(msg, baseCfg('full'), c)).toEqual({ title: 'Alice', body: 'hi there' });
  });
  it('minimal → sender only', () => {
    expect(buildNotification(msg, baseCfg('minimal'), c)).toEqual({ title: 'Alice', body: '' });
  });
  it('generic → New message', () => {
    expect(buildNotification(msg, baseCfg('generic'), c)).toEqual({ title: 'Skattr', body: 'New message' });
  });
});
