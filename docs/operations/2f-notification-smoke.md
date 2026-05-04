# Phase 2.F Notification + Tray Smoke Checklist

Manual cross-OS verification of the notification + tray + logs +
wipe surfaces. Run on each platform before Phase 2.G ships
installers.

**Setup:** install Skattr from a dev build; create two paired
contacts (Alice and Bob); send a message from B → A while A's UI
is in each state below.

## Notifications — Linux (X11 / GNOME 45+)

- [ ] Window focused, Alice conversation active, msg from Alice: NO notification
- [ ] Window focused, Bob conversation active, msg from Alice: notification fires
- [ ] Window blurred: notification fires
- [ ] Window minimised: notification fires
- [ ] Notifications mode = Full: title = sender, body = message preview
- [ ] Notifications mode = Minimal: title = sender, body = empty
- [ ] Notifications mode = Generic: title = "Skattr", body = "New message"
- [ ] Notifications mode = Off: no notification regardless of state
- [ ] Per-contact mute (bell toggle in ContactDetailsPanel): no notification, no unread badge
- [ ] Click notification: window focuses + opens that conversation (Linux only — XDG hint)

## Notifications — Linux (Wayland)

Same checklist. Two known limitations on bare Wayland:
- Tray may be absent (no StatusNotifier protocol on the desktop). The daemon logs a warning and the close button falls back to "quit"; verify by checking the daemon log for `tray init failed`.
- Click-to-focus from notifications relies on the conversation_id XDG hint, which not all Wayland notifiers honour. Verify per DE.

## Notifications — macOS 14+

- [ ] All Linux items (the focus-aware suppression rules apply uniformly)
- [ ] Dock-bounce notification respects "Do Not Disturb"
- [ ] Tray icon in menu bar (top-right)
- [ ] **Click-to-focus from notification:** macOS routes notification clicks
  through `notify-rust`'s response API, NOT the XDG hint Linux uses. In 2.F's
  first cut this is not wired — clicking the notification surfaces the app
  but doesn't navigate to the conversation. Verify behaviour matches expectation
  (don't fail the smoke test on this — it's a known follow-up).

## Notifications — Windows 11

- [ ] All Linux items
- [ ] Notifications appear in Action Center
- [ ] Tray icon in system tray (bottom-right); right-click shows menu
- [ ] Click-to-focus from notification: same caveat as macOS (separate notify-rust API)

## Tray menu

- [ ] Tray icon present (or absent on Wayland with the documented fallback)
- [ ] Left-click toggles window visibility (show ↔ hide)
- [ ] Menu shows: Skattr (header) / Show window / Tor: <status> / Unread: <n> / Quit
- [ ] Tor status updates as the daemon bootstraps (placeholder until the live tap lands)
- [ ] Unread updates reflect new messages (placeholder until live tap lands)
- [ ] Quit: daemon process exits cleanly (no orphan processes; verify via `ps`)
- [ ] Close-button hides to tray when `ui.close_to_tray = true` (default)
- [ ] Close-button quits when `ui.close_to_tray = false`
- [ ] First close-to-tray click shows a toast: "Skattr is still running in
      the tray. Quit from the tray menu to stop the daemon."

## Logs viewer (Settings → Advanced)

- [ ] Open: latest ≤ 500 records appear, colour-coded by level
- [ ] Live tail: send a message and verify a new INFO record appears within 1s
- [ ] No 64-char hex blobs above DEBUG level (visual inspection)
- [ ] No `*.onion` strings above DEBUG level
- [ ] Toggle "Persist logs to disk" → toast says "effective on next daemon restart"
- [ ] Restart daemon with persist=true → file appears at `${data_dir}/logs/skattr.log`
- [ ] Toggle off + restart: file remains, no new lines appended
- [ ] "Copy logs" button: clipboard receives the visible records as TSV-ish text

## Wipe (Settings → Advanced → Danger zone)

- [ ] Click "Delete all data and quit" → first confirm appears
- [ ] Confirm → second confirm appears
- [ ] Confirm → daemon shuts down within ~1s, data_dir is removed, app exits
- [ ] Restart Skattr: first-run wizard appears (verifies wipe was complete)

## Reporting issues

If any item fails, note the OS + DE + Tauri version (`cargo
tauri info`) and file under `docs/operations/issues/`.
