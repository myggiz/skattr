// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, expect, test, vi, beforeEach } from "vitest";
import { render } from "@testing-library/svelte";
import { tick } from "svelte";

// convertFileSrc + invoke come from @tauri-apps/api/core.
const { invokeMock, convertFileSrcMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  convertFileSrcMock: vi.fn((p: string) => `asset://localhost/${p}`),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
  convertFileSrc: convertFileSrcMock,
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
import { attachments, applyReceived, applyProgress } from "$lib/stores/attachments";
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
  invokeMock.mockReset();
  invokeMock.mockResolvedValue({
    attachment_id: AID, filename: "photo.jpg", mime: "image/jpeg", total_size: 2048,
  });
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

  test("renders an inline <img> when complete + image", async () => {
    const { container, findByText } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("photo.jpg");
    applyReceived(AID, { filename: "photo.jpg", mime: "image/jpeg", size: 2048, path: "/dl/photo.jpg" });
    await tick();
    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    expect(convertFileSrcMock).toHaveBeenCalledWith("/dl/photo.jpg");
  });

  test("complete + non-image shows Open/Reveal, no img", async () => {
    invokeMock.mockResolvedValue({
      attachment_id: AID, filename: "doc.pdf", mime: "application/pdf", total_size: 10,
    });
    const { container, findByText, getByRole } = render(FileAttachmentBubble, {
      props: { record: fileRecord("incoming") },
    });
    await findByText("doc.pdf");
    applyReceived(AID, { filename: "doc.pdf", mime: "application/pdf", size: 10, path: "/dl/doc.pdf" });
    await tick();
    expect(container.querySelector("img")).toBeNull();
    expect(getByRole("button", { name: /open/i })).toBeTruthy();
    expect(getByRole("button", { name: /reveal/i })).toBeTruthy();
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
    expect(queryByText(/unavailable/i)).toBeNull();
    expect(queryByText("myfile.pdf")).not.toBeNull();
  });
});
