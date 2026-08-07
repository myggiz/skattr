// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test, vi, beforeEach } from "vitest";
import { render } from "@testing-library/svelte";
import { tick } from "svelte";

// invoke comes from @tauri-apps/api/core (convertFileSrc removed in Task 6).
const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
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
import { attachments, applyProgress, markAvailable } from "$lib/stores/attachments";
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
  };
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
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("outgoing") },
    });
    await findByText("photo.jpg");
    expect(container.querySelector(".progress")).toBeNull();
    expect(container.querySelector(".icon")).not.toBeNull();
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
});
