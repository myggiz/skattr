// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test, vi, beforeEach } from "vitest";
import { render, fireEvent, screen } from "@testing-library/svelte";
import { tick } from "svelte";

// invoke comes from @tauri-apps/api/core (convertFileSrc removed in Task 6).
const { invokeMock, convertFileSrcMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  convertFileSrcMock: vi.fn((p: string) => `asset://localhost/${p}`),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
  convertFileSrc: convertFileSrcMock,
}));

// Mock ipcClient so we can inspect attachment_available and other IPC calls.
const { ipcRequestMock } = vi.hoisted(() => ({
  ipcRequestMock: vi.fn(),
}));
vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: { request: ipcRequestMock },
}));

// Mock @tauri-apps/plugin-dialog save function.
const { saveMock } = vi.hoisted(() => ({
  saveMock: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: saveMock,
  ask: vi.fn().mockResolvedValue(false),
}));

// Mock decodeManifestMemo to bypass the module-level memo so each test
// controls invokeMock independently.
vi.mock("$lib/attachments", async (importOriginal) => {
  const orig = await importOriginal<typeof import("$lib/attachments")>();
  return {
    ...orig,
    decodeManifestMemo: (_mid: string, fileKind: Extract<import("$lib/ipc/types").Kind, { kind: "file" }>) =>
      orig.decodeManifest(fileKind),
  };
});

import FileAttachmentBubble from "./FileAttachmentBubble.svelte";
import {
  attachments,
  applyFailed,
  applyProgress,
  attachmentFor,
  markAvailable,
} from "$lib/stores/attachments";
import { delivery, recordDeliveryStatus } from "$lib/stores/delivery";
import type { MessageRecord } from "$lib/ipc/types";

const AID = "ab".repeat(16);

function fileRecord(direction: "incoming" | "outgoing"): MessageRecord {
  return {
    row_id: 1n,
    message_id: "cd".repeat(16),
    contact: "ef".repeat(32),
    direction,
    kind: { kind: "file", manifest: [1, 2, 3] as unknown as string },
    mls_generation: 0n,
    ts_daemon_recv: 1_700_000_000n,
    ts_envelope: 1_700_000_000n,
    delivered_at: null,
    dismissed_at: null,
    failed_reason: null,
  };
}

function makeOutgoingFile(overrides: Partial<MessageRecord> = {}): MessageRecord {
  return { ...fileRecord("outgoing"), ...overrides };
}

beforeEach(() => {
  attachments.set(new Map());
  delivery.set(new Map());
  invokeMock.mockReset();
  ipcRequestMock.mockReset();
  saveMock.mockReset();
  // Discriminate by command: decode_attachment_manifest → manifest.
  invokeMock.mockImplementation((cmd: string, args?: unknown) => {
    if (cmd === "decode_attachment_manifest") {
      const manifest = (args as { manifest: number[] })?.manifest ?? [];
      if (manifest.length === 0) return Promise.reject(new Error("empty manifest"));
      return Promise.resolve({ attachment_id: AID, filename: "photo.jpg", mime: "image/jpeg", total_size: 2048 });
    }
    return Promise.resolve(null);
  });
  // Default: attachment_available returns not available.
  ipcRequestMock.mockResolvedValue({ resp: "ok", data: { result: "attachment_availability", data: { available: false } } });
});

describe("FileAttachmentBubble", () => {
  test("renders the decoded filename as a static card", async () => {
    const { findByText } = render(FileAttachmentBubble, { props: { record: fileRecord("incoming") } });
    expect(await findByText("photo.jpg")).toBeTruthy();
  });

  test("shows a progress bar while receiving", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    applyProgress(AID, 1, 4);
    await tick();
    expect(container.querySelector(".progress")).not.toBeNull();
  });

  test("shows the indeterminate 'Downloading…' state when total is 0", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    applyProgress(AID, 0, 0); // received=0, total=0 → indeterminate
    await tick();
    const progress = container.querySelector(".progress");
    expect(progress).not.toBeNull();
    expect(progress?.classList.contains("indeterminate")).toBe(true);
    await findByText("Downloading…");
    // No determinate percentage bar in the indeterminate state.
    expect(container.querySelector(".progress .bar")).toBeNull();
  });

  test("complete + available receiver bubble shows Open and Save… buttons, no <img>", async () => {
    const { container, findByText, getByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    markAvailable(AID, { filename: "photo.jpg", mime: "image/jpeg", size: 2048 });
    await tick();
    // No inline image preview (Task 6 decision: images are plain file cards).
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector(".preview")).toBeNull();
    // Open and Save… buttons are present.
    expect(getByRole("button", { name: /open/i })).toBeTruthy();
    expect(getByRole("button", { name: /save/i })).toBeTruthy();
  });

  // #175: after saving there was no lasting sign it happened — only a toast
  // that disappears, leaving the user with no answer to "where did it go?".
  test("after a successful save, shows a Saved marker and Show, and relabels the button", async () => {
    saveMock.mockResolvedValueOnce("/home/u/Downloads/photo.jpg");
    ipcRequestMock.mockResolvedValue({ resp: "ok", data: { result: "ok" } });

    const { findByText, getByRole, queryByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    markAvailable(AID, { filename: "photo.jpg", mime: "image/jpeg", size: 2048 });
    await tick();

    // Before saving there is no marker, and the button offers the first save.
    expect(queryByText("✓ Saved")).toBeNull();
    expect(getByRole("button", { name: /save decrypted file/i })).toBeTruthy();

    await fireEvent.click(getByRole("button", { name: /save decrypted file/i }));
    await tick();
    await tick();

    // The marker reports state and does not rely on colour to do it.
    expect(await findByText("✓ Saved")).toBeTruthy();
    // Show answers "where did it go?" once the toast is gone.
    expect(getByRole("button", { name: /show saved file in folder/i })).toBeTruthy();
    // The action button still describes the action, so a second save to a
    // different destination stays obviously available.
    expect(getByRole("button", { name: /save another copy/i })).toBeTruthy();
  });

  test("complete non-image shows Open and Save… buttons, no <img>", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "decode_attachment_manifest") {
        return Promise.resolve({ attachment_id: AID, filename: "doc.pdf", mime: "application/pdf", total_size: 10 });
      }
      return Promise.resolve(null);
    });
    const { container, findByText, getByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("doc.pdf");
    markAvailable(AID, { filename: "doc.pdf", mime: "application/pdf", size: 10 });
    await tick();
    expect(container.querySelector("img")).toBeNull();
    expect(getByRole("button", { name: /open/i })).toBeTruthy();
    expect(getByRole("button", { name: /save/i })).toBeTruthy();
  });

  test("clicking Save… invokes dialog save then save_attachment IPC", async () => {
    const DEST = "/home/user/Downloads/photo.jpg";
    saveMock.mockResolvedValue(DEST);
    ipcRequestMock.mockImplementation((cmd: unknown) => {
      const c = cmd as { cmd: string };
      if (c.cmd === "attachment_available") {
        return Promise.resolve({ resp: "ok", data: { result: "attachment_availability", data: { available: false } } });
      }
      if (c.cmd === "save_attachment") {
        return Promise.resolve({ resp: "ok", data: { result: "ok" } });
      }
      return Promise.resolve({ resp: "ok", data: { result: "ok" } });
    });

    const { findByText, getByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    markAvailable(AID, { filename: "photo.jpg", mime: "image/jpeg", size: 2048 });
    await tick();

    const saveBtn = getByRole("button", { name: /save/i });
    saveBtn.click();
    await tick();
    await new Promise((r) => setTimeout(r, 0));
    await tick();

    expect(saveMock).toHaveBeenCalledOnce();
    expect(ipcRequestMock).toHaveBeenCalledWith(
      expect.objectContaining({ cmd: "save_attachment", attachment_id: AID, dest_path: DEST }),
    );
  });

  test("on mount of received-but-not-in-store bubble, issues attachment_available and calls markAvailable on true", async () => {
    // Set up ipcRequestMock to return available=true for attachment_available.
    ipcRequestMock.mockImplementation((cmd: unknown) => {
      const c = cmd as { cmd: string };
      if (c.cmd === "attachment_available") {
        return Promise.resolve({
          resp: "ok",
          data: { result: "attachment_availability", data: { available: true } },
        });
      }
      return Promise.resolve({ resp: "ok", data: { result: "ok" } });
    });

    const { findByText, getByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    // Let the $effect run.
    await new Promise((r) => setTimeout(r, 0));
    await tick();

    // The component should have queried attachment_available.
    expect(ipcRequestMock).toHaveBeenCalledWith(
      expect.objectContaining({ cmd: "attachment_available", attachment_id: AID }),
    );
    // Since available=true, the store should now show Open and Save… buttons.
    expect(getByRole("button", { name: /open/i })).toBeTruthy();
    expect(getByRole("button", { name: /save/i })).toBeTruthy();
  });

  test("outgoing bubble shows a delivery icon and no progress bar", async () => {
    // Needs an actual delivery record: with none at all the bubble now
    // deliberately renders no icon, because "we have no record" must not look
    // like "in flight" (#176).
    recordDeliveryStatus("cd".repeat(16), "Queued");
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    expect(container.querySelector(".progress")).toBeNull();
    expect(container.querySelector(".icon")).not.toBeNull();
  });

  // #172: viewing a received image in-app. The bytes are encrypted at rest, so
  // a preview must go through the same explicit decrypt as Open — plaintext
  // exists only because the user asked, and #52 wipes cache/open on exit.
  test("an image attachment offers View, which decrypts and renders it inline", async () => {
    ipcRequestMock.mockImplementation((req: { cmd: string }) => {
      if (req.cmd === "open_attachment") {
        return Promise.resolve({
          resp: "ok",
          data: { result: "attachment_decrypted", data: { path: "/data/cache/open/ab/photo.jpg" } },
        });
      }
      return Promise.resolve({
        resp: "ok",
        data: { result: "attachment_availability", data: { available: false } },
      });
    });

    const { container, findByText, getByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    markAvailable(AID, { filename: "photo.jpg", mime: "image/jpeg", size: 2048 });
    await tick();

    // Nothing decrypted yet.
    expect(container.querySelector("img")).toBeNull();

    await fireEvent.click(getByRole("button", { name: /view/i }));
    await tick();
    await tick();

    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    expect(img?.getAttribute("src")).toContain("/data/cache/open/ab/photo.jpg");
    expect(
      ipcRequestMock.mock.calls.some((c) => (c[0] as { cmd: string }).cmd === "open_attachment"),
    ).toBe(true);
  });

  test("a non-image attachment offers no View action", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "decode_attachment_manifest") {
        return Promise.resolve({
          attachment_id: AID,
          filename: "notes.pdf",
          mime: "application/pdf",
          total_size: 2048,
        });
      }
      return Promise.resolve(null);
    });

    const { findByText, queryByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("notes.pdf");
    markAvailable(AID, { filename: "notes.pdf", mime: "application/pdf", size: 2048 });
    await tick();

    expect(queryByRole("button", { name: /view/i })).toBeNull();
  });

  test("an image over the inline-preview ceiling offers no View action", async () => {
    const huge = 64 * 1024 * 1024;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "decode_attachment_manifest") {
        return Promise.resolve({
          attachment_id: AID,
          filename: "huge.jpg",
          mime: "image/jpeg",
          total_size: huge,
        });
      }
      return Promise.resolve(null);
    });

    const { findByText, queryByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("huge.jpg");
    markAvailable(AID, { filename: "huge.jpg", mime: "image/jpeg", size: huge });
    await tick();

    expect(queryByRole("button", { name: /view/i })).toBeNull();
  });

  test("decode failure shows the unavailable card", async () => {
    invokeMock.mockRejectedValue(new Error("bad version"));
    const { findByText } = render(FileAttachmentBubble, { props: { record: fileRecord("incoming") } });
    expect(await findByText(/unavailable/i)).toBeTruthy();
  });

  test("optimistic outgoing bubble (empty manifest) shows the picked filename, not 'unavailable'", async () => {
    // Faithful to the real Rust decoder: an empty manifest fails to decode.
    // The optimistic placeholder must skip decoding and fall through to the
    // file card carrying the picked filename/size, never the unavailable card.
    invokeMock.mockImplementation((_cmd: string, args: { manifest: number[] }) =>
      args.manifest.length === 0
        ? Promise.reject(new Error("empty manifest"))
        : Promise.resolve({
            attachment_id: AID, filename: "photo.jpg", mime: "image/jpeg", total_size: 2048,
          }),
    );
    const optimistic = {
      ...fileRecord("outgoing"),
      message_id: "00".repeat(16),
      kind: { kind: "file", manifest: [] as unknown as string },
      __attachName: "myfile.pdf",
      __attachSize: 4096,
    } as unknown as MessageRecord;
    const { findByText, queryByText } = render(FileAttachmentBubble, { props: { record: optimistic } });
    expect(await findByText("myfile.pdf")).toBeTruthy();
    // Let any decode attempt settle. Without the guard the empty-manifest
    // decode rejects on the next microtask and flips to the unavailable card.
    await new Promise((r) => setTimeout(r, 0));
    await tick();
    // The guard short-circuits before the decode boundary, so invoke() (reached
    // via decodeManifestMemo -> decodeManifest -> invoke) is never called for an
    // empty manifest. Without the guard this fires and rejects -> unavailable.
    expect(invokeMock).not.toHaveBeenCalled();
    expect(queryByText(/unavailable/i)).toBeNull();
    expect(queryByText("myfile.pdf")).not.toBeNull();
  });

  test("outgoing bubble shows chunk progress, not Delivered, while serving", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    applyProgress(AID, 3, 12);
    await tick();
    // Progress row rendered with the served/total figure.
    expect(container.querySelector(".progress")).not.toBeNull();
    await findByText("Sending 3/12");
    // Must NOT claim the transfer finished.
    expect(container.textContent).not.toContain("Delivered");
  });

  // #177: a just-sent bubble is the optimistic placeholder promoted in place.
  // It carries an EMPTY manifest (the real bytes only exist after SendFile
  // returns), so there is nothing to decode and the bubble cannot learn its
  // attachment id that way. sendFile does know it — from FileQueued — and
  // carries it on the record as __attachId. Without that link the bubble reads
  // the transfer store under `null` and can never show Delivered, no matter how
  // long the transfer runs; it only appears once a conversation reload replaces
  // the placeholder with the real record.
  test("promoted outgoing bubble with an empty manifest still shows Delivered", async () => {
    const promoted = {
      ...fileRecord("outgoing"),
      kind: { kind: "file", manifest: [] as unknown as string },
      __tempId: "tmp-177",
      __optimistic: false,
      __attachId: AID,
      __attachName: "photo.jpg",
      __attachSize: 2048,
    };
    const { findByText } = render(FileAttachmentBubble, {
      props: { record: promoted as unknown as MessageRecord },
    });
    await tick();

    // The sender's completion signal: a progress event with received == total.
    applyProgress(AID, 8, 8);

    expect(await findByText("Delivered")).toBeTruthy();
  });

  test("outgoing bubble shows Delivered once the transfer completes", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    // Drive the store the way the real attachment_progress dispatcher does —
    // markAvailable is a receiver-only mutation no outgoing code path performs.
    applyProgress(AID, 12, 12);
    await tick();
    await findByText("Delivered");
    // The in-flight progress row is gone.
    expect(container.querySelector(".progress")).toBeNull();
  });

  test("manifest ack alone does not claim the file transferred (#114 regression)", async () => {
    // The manifest message is MLS-acked before any chunk moves. With no
    // transfer state, the bubble may show the delivery icon but must not
    // assert the transfer completed.
    // ts-rs emits DeliveryStatus as "Queued" | "Delivered" | "Deposited"
    // | { "Failed": string } — the capitalised literal is the wire value.
    recordDeliveryStatus("cd".repeat(16), "Delivered");
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    await tick();
    expect(container.textContent).not.toContain("Delivered");
    expect(container.querySelector(".progress")).toBeNull();
    // The pre-transfer fallback icon is still rendered, but the manifest ack
    // must be capped: a "Delivered" wire status may never render as the
    // delivered checkmark on a file bubble before the transfer completes.
    expect(container.querySelector(".icon")).not.toBeNull();
    expect(container.querySelector(".icon.delivered")).toBeNull();
    expect(container.querySelector(".icon.sent")).not.toBeNull();
  });

  // #176: the transfer store is session-scoped, so after a restart a sent
  // attachment has no in-memory state at all. The daemon persisted the
  // completion on the out row; the bubble must ask for it rather than
  // rendering the in-flight clock forever.
  test("post-restart outgoing bubble rehydrates Delivered from the daemon", async () => {
    ipcRequestMock.mockImplementation((req: { cmd: string }) => {
      if (req.cmd === "attachment_status") {
        return Promise.resolve({
          resp: "ok",
          data: {
            result: "attachment_status",
            data: { report: { direction: "Out", state: "Complete" } },
          },
        });
      }
      return Promise.resolve({
        resp: "ok",
        data: { result: "attachment_availability", data: { available: false } },
      });
    });

    const { findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });

    expect(await findByText("Delivered")).toBeTruthy();
  });

  // #176: "we have no record" must not animate as though something were still
  // happening. With no persisted row and no delivery status, the bubble
  // asserts nothing.
  test("an outgoing bubble with no record shows neither Delivered nor an in-flight icon", async () => {
    ipcRequestMock.mockImplementation((req: { cmd: string }) => {
      if (req.cmd === "attachment_status") {
        return Promise.resolve({
          resp: "ok",
          data: { result: "attachment_status", data: { report: null } },
        });
      }
      return Promise.resolve({
        resp: "ok",
        data: { result: "attachment_availability", data: { available: false } },
      });
    });

    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    await tick();
    await tick();

    expect(container.textContent).not.toContain("Delivered");
    expect(container.querySelector(".progress")).toBeNull();
    expect(container.querySelector(".icon")).toBeNull();
  });

  test("a failed transfer offers Retry, which re-arms it (#144)", async () => {
    const { container, findByText, getByLabelText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    applyProgress(AID, 2, 8);
    applyFailed(AID, "request timeout");
    await tick();
    await findByText("⚠️ request timeout");

    ipcRequestMock.mockResolvedValueOnce({ resp: "ok", data: { result: "ok" } });
    getByLabelText("Retry transfer").click();
    await tick();
    await tick();

    expect(ipcRequestMock).toHaveBeenCalledWith(
      expect.objectContaining({ cmd: "retry_attachment", attachment_id: AID }),
    );
    // Re-armed: the error is gone, the waiting state is shown, and the chunks
    // already received are kept (a retry resumes, it does not restart).
    const state = attachmentFor(AID);
    expect(state).toMatchObject({ status: "queued", retrying: true, received: 2 });
    expect(state?.reason).toBeUndefined();
    await findByText("Retrying — waiting for the sender…");
    expect(container.querySelector(".failed")).toBeNull();
  });

  test("a rejected retry leaves the failed state alone (#144)", async () => {
    const { findByText, getByLabelText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    applyFailed(AID, "sender nack reason 1");
    await tick();

    ipcRequestMock.mockResolvedValueOnce({ resp: "err", error: { kind: "invalid_argument" } });
    getByLabelText("Retry transfer").click();
    await tick();
    await tick();

    expect(attachmentFor(AID)).toMatchObject({ status: "failed", reason: "sender nack reason 1" });
    await findByText("⚠️ sender nack reason 1");
  });

  // Task 8: a Kind::File message rides the same outbox as text, so a send
  // failure must not leave the file bubble showing an eternal clock either.
  // Resend is explicitly out of scope (spec §7) — the original path may no
  // longer exist on the sender's filesystem.
  test("an outgoing file that failed to send shows the reason and Dismiss, but no Resend", () => {
    const rec = makeOutgoingFile({ failed_reason: "Not delivered — no mailbox." });
    render(FileAttachmentBubble, { props: { record: rec } });

    expect(screen.getByText(/no mailbox/i)).toBeTruthy();
    expect(screen.getByRole("button", { name: /dismiss/i })).toBeTruthy();
    // Resend needs the original path, which may be gone (spec §7).
    expect(screen.queryByRole("button", { name: /resend/i })).toBeNull();
  });
});
