// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, it, expect } from "vitest";
import { errorMessage } from "./errors";
import type { IpcError } from "./types";

describe("errorMessage", () => {
  it("maps invite_expired", () => {
    const e: IpcError = { err: "daemon", data: { kind: "invite_expired" } };
    expect(errorMessage(e)).toMatch(/expired/i);
  });
  it("maps invite_consumed", () => {
    const e: IpcError = { err: "daemon", data: { kind: "invite_consumed" } };
    expect(errorMessage(e)).toMatch(/already been used/i);
  });
  it("maps invite_signature_invalid", () => {
    const e: IpcError = { err: "daemon", data: { kind: "invite_signature_invalid" } };
    expect(errorMessage(e)).toMatch(/verif/i);
  });
  it("maps delivery_timeout to an offline hint", () => {
    const e: IpcError = { err: "daemon", data: { kind: "delivery_timeout" } };
    expect(errorMessage(e)).toMatch(/offline|reach/i);
  });
  it("uses the message for invalid_argument", () => {
    const e: IpcError = { err: "daemon", data: { kind: "invalid_argument", data: { message: "bad path" } } };
    expect(errorMessage(e)).toBe("bad path");
  });
  it("falls back generically for internal", () => {
    const e: IpcError = { err: "internal", data: "boom" };
    expect(errorMessage(e)).toMatch(/something went wrong/i);
  });
});
