import { writable, type Readable } from 'svelte/store';

interface FocusState {
  windowFocused: boolean;
  activeContactId: string | null;
}

const _state = writable<FocusState>({ windowFocused: true, activeContactId: null });

export const focus: Readable<FocusState> = { subscribe: _state.subscribe };

export function setActiveContact(id: string | null) {
  _state.update((s) => ({ ...s, activeContactId: id }));
}

export function setWindowFocused(focused: boolean) {
  _state.update((s) => ({ ...s, windowFocused: focused }));
}

// Wire up to the browser focus events on init.
if (typeof window !== 'undefined') {
  window.addEventListener('focus', () => setWindowFocused(true));
  window.addEventListener('blur', () => setWindowFocused(false));
}
