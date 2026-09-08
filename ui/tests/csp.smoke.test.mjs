import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import test from "node:test";

test("production bundle is compatible with the configured CSP", async () => {
  const tauri = JSON.parse(await readFile("../crates/desktop/tauri.conf.json", "utf8"));
  const csp = tauri.app.security.csp;
  assert.match(csp, /script-src 'self'/);
  assert.match(csp, /style-src 'self'/);
  assert.doesNotMatch(csp, /'unsafe-inline'|'unsafe-eval'/);
  assert.match(csp, /connect-src 'self' ipc: http:\/\/ipc\.localhost/);

  const html = await readFile("dist/index.html", "utf8");
  const scripts = [...html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi)];
  assert.ok(scripts.length > 0, "the production entry script must exist");
  for (const script of scripts) {
    assert.match(script[1], /\bsrc=/, "scripts must be external under script-src 'self'");
    assert.equal(script[2].trim(), "", "inline script bodies are forbidden");
  }
  assert.doesNotMatch(html, /<style\b|\son[a-z]+\s*=/i);
  assert.doesNotMatch(html, /(?:src|href)=["']https?:/i);

  const assets = await readdir("dist/assets");
  assert.ok(assets.some(name => name.endsWith(".js")), "the JS bundle must be emitted");
  assert.ok(assets.some(name => name.endsWith(".css")), "the CSS bundle must be emitted");
  for (const name of assets.filter(value => value.endsWith(".js"))) {
    const source = await readFile(`dist/assets/${name}`, "utf8");
    assert.doesNotMatch(source, /\beval\s*\(|\bnew\s+Function\s*\(/);
  }
});
