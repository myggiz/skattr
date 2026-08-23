// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// #172: the inline image view was dead on arrival — not because the feature
// was wrong, but because this app enforces TWO Content-Security-Policies:
//
//   1. the header policy in `crates/ui/tauri.conf.json`
//   2. the `<meta http-equiv>` policy in `src/app.html`
//
// Both apply, and a resource must satisfy BOTH. Every Tauri doc and example
// points at (1), so (1) listed the asset protocol and (2) did not — and (2),
// being stricter, refused every `asset://localhost` image. The webview only
// says "does not appear in the img-src directive", naming neither policy.
//
// 3.D shipped the same mismatch and its preview could never have worked;
// nothing caught it because unit tests mock `convertFileSrc` and never load a
// real image, and the e2e suite runs against a mock backend with no assets.
//
// @vitest-environment node

import { expect, test, vi } from "vitest";

const fsModule = (await vi.importActual("fs")) as {
  readFileSync: (path: string, encoding: string) => string;
};
const urlModule = (await vi.importActual("url")) as {
  fileURLToPath: (u: URL | string) => string;
};
// fileURLToPath, not `.pathname`: the latter stays percent-encoded, so a
// checkout path containing a space or a non-ASCII character would be read as a
// literal "%20" path and fail.
const read = (rel: string) =>
  fsModule.readFileSync(urlModule.fileURLToPath(new URL(rel, import.meta.url)), "utf-8");

/**
 * Extract one directive's source list as discrete tokens.
 *
 * Tokens, not a substring: `toContain("asset://localhost")` is also satisfied
 * by `asset://localhost.example`, so a substring check could pass while the
 * policy names a different origin entirely — precisely the failure this test
 * exists to catch.
 */
function sources(csp: string, name: string): string[] {
  const found = csp
    .split(";")
    .map((d) => d.trim())
    .find((d) => d === name || d.startsWith(`${name} `));
  if (!found) return [];
  return found.split(/\s+/).slice(1);
}

const metaCsp = (() => {
  const html = read("../app.html");
  return html.match(/http-equiv="Content-Security-Policy"\s+content="([^"]+)"/)?.[1] ?? "";
})();

const configCsp = (() => {
  const conf = JSON.parse(read("../../../tauri.conf.json")) as {
    app: { security: { csp: string } };
  };
  return conf.app.security.csp;
})();

test("both CSPs are actually present", () => {
  expect(metaCsp, "meta CSP not found in app.html").not.toBe("");
  expect(configCsp, "csp not found in tauri.conf.json").not.toBe("");
});

// The asset protocol is served as `asset://localhost/...` on Linux/macOS and
// `http://asset.localhost/...` on Windows, so both forms are required. The bare
// `asset:` scheme-source alone is not sufficient in WebKitGTK.
for (const [label, csp] of [
  ["app.html meta CSP", metaCsp],
  ["tauri.conf.json header CSP", configCsp],
] as const) {
  test(`${label} allows the asset protocol for images`, () => {
    const imgSrc = sources(csp, "img-src");
    expect(imgSrc.length, `${label} has no img-src directive`).toBeGreaterThan(0);
    // Linux/macOS serve the protocol as asset://localhost/…, Windows as
    // http://asset.localhost/…, so both are required.
    expect(imgSrc, `${label} img-src must allow asset://localhost`).toContain("asset://localhost");
    expect(imgSrc, `${label} img-src must allow http://asset.localhost`).toContain(
      "http://asset.localhost",
    );
  });
}
