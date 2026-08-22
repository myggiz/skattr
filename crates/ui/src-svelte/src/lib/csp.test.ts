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
const read = (rel: string) =>
  fsModule.readFileSync(new URL(rel, import.meta.url).pathname, "utf-8");

/** Extract one directive's source list from a CSP string. */
function directive(csp: string, name: string): string {
  const found = csp
    .split(";")
    .map((d) => d.trim())
    .find((d) => d.startsWith(`${name} `));
  return found ?? "";
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
    const imgSrc = directive(csp, "img-src");
    expect(imgSrc, `${label} has no img-src directive`).not.toBe("");
    expect(imgSrc, `${label} img-src must allow asset://localhost`).toContain("asset://localhost");
    expect(imgSrc, `${label} img-src must allow http://asset.localhost`).toContain(
      "http://asset.localhost",
    );
  });
}
