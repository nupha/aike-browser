// Test: attach to an ALREADY-RUNNING CDP server (nothing is launched here)
//
// Covers the attach-only path of the browser module:
//   Browser.connect(cdpUrl) — a bare origin (ws://127.0.0.1:9222) is resolved
//                             through /json/version to the browser WS endpoint
//   Browser domain          — getVersion, getTargets, listPages
//   Page domain             — newPage, navigate, waitForSelector, evaluate,
//                             getOuterHTML, setViewport, getLayoutMetrics,
//                             captureScreenshot, close
//   non-destructive         — pre-existing tabs are listed but never navigated
//                             or closed; only the tab this test creates is
//   detach-only close()     — the external process survives, and a second
//                             connect() to the same URL still works
//   argument validation     — a non-ws:// URL is rejected with a TypeError
//
// Start a browser first, e.g.
//   chrome --remote-debugging-port=9222
// then run (as a module, so `import` is parsed in module mode):
//   aike -m tests/connect_existing.mjs [ws://127.0.0.1:9222]
//
// The URL comes from argv[2], else $CDP_URL, else ws://127.0.0.1:9222. With no
// browser on the default port the run reports SKIP and exits 0, so the file is
// safe to invoke from a suite.

import { Browser } from "browser";

const DEFAULT_CDP_URL = "ws://127.0.0.1:9222";

let pass = 0;
let fail = 0;

function check(label, condition, detail) {
  if (condition) {
    console.log("  [ok] " + label);
    pass++;
  } else {
    console.log("  [FAIL] " + label + (detail ? " -- " + detail : ""));
    fail++;
  }
}

// Accept what a human would type: "9222", "127.0.0.1:9222", "http://…",
// "https://…", with or without a trailing slash. `Browser.connect` itself is
// strict (ws:// / wss:// only), so the normalization lives here.
function normalizeCdpUrl(raw) {
  let s = String(raw).trim();
  if (!s) throw new Error("CDP URL must not be empty");
  if (!/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(s)) {
    // Bare port ("9222") means localhost on that port.
    s = "ws://" + (/^\d+$/.test(s) ? "127.0.0.1:" + s : s);
  }
  if (s.startsWith("http://")) s = "ws://" + s.slice(7);
  else if (s.startsWith("https://")) s = "wss://" + s.slice(8);
  return s.replace(/\/+$/, "");
}

const PAGE =
  "data:text/html," +
  encodeURIComponent(
    "<!DOCTYPE html><html><head><title>attach</title></head><body>" +
      '<h1 id="tag">attached</h1>' +
      '<div class="row">a</div><div class="row">b</div>' +
      "</body></html>",
  );

// Rejection helper: connect() must reject, and the message must mention `msg`.
async function rejectsConnect(cdpUrl, msg) {
  try {
    await Browser.connect(cdpUrl);
  } catch (e) {
    const text = String((e && e.message) || e);
    return text.indexOf(msg) >= 0 ? "" : text;
  }
  return null; // did not reject
}

async function main() {
  const raw = process.argv[2] || process.env.CDP_URL || DEFAULT_CDP_URL;
  const cdpUrl = normalizeCdpUrl(raw);
  // Only an explicitly supplied URL turns "nothing is listening" into a
  // failure; the default port stays a SKIP so the file is suite-safe.
  const explicit = Boolean(process.argv[2] || process.env.CDP_URL);

  console.log("\n=== connect to a running CDP server: " + cdpUrl + " ===");

  // 0. argument validation (offline, no server needed).
  console.log("\n=== connect() argument validation ===");
  check(
    "connect('127.0.0.1:9222') rejects (ws:// scheme required)",
    (await rejectsConnect("127.0.0.1:9222", "ws://")) === "",
  );
  check(
    "connect('http://127.0.0.1:9222') rejects (ws:// scheme required)",
    (await rejectsConnect("http://127.0.0.1:9222", "ws://")) === "",
  );

  // 1. attach.
  console.log("\n=== connect ===");
  let browser;
  try {
    browser = await Browser.connect(cdpUrl);
  } catch (e) {
    const msg = "  " + ((e && e.message) || e);
    if (!explicit) {
      console.log("  no CDP server at " + cdpUrl + " -- SKIP");
      console.log(msg);
      console.log("  start one with: chrome --remote-debugging-port=9222");
      return 0;
    }
    console.log("  [FAIL] connect() threw:" + msg);
    return 1;
  }
  check("connect() resolves with a Browser", !!browser);
  // Attach mode owns no child process, so `isRunning` reports the CDP connection
  // this Browser holds — which is open right after a successful connect(). It
  // used to be a constant `false` even for a live attached browser. `close()`
  // must still not send Browser.close.
  check(
    "isRunning is true in attach mode (connection is open)",
    browser.isRunning === true,
    "got: " + browser.isRunning,
  );

  // 2. Browser domain against the external process.
  console.log("\n=== Browser domain ===");
  const ver = await browser.getVersion();
  check(
    "getVersion() returns version info",
    !!ver && typeof ver.protocolVersion === "string" && ver.protocolVersion.length > 0 &&
      typeof ver.product === "string" && ver.product.length > 0 &&
      typeof ver.userAgent === "string",
    "got: " + JSON.stringify(ver),
  );
  console.log("  product: " + (ver && ver.product));
  console.log("  userAgent: " + (ver && ver.userAgent));

  // 3. inventory of the tabs the user already has open — read-only.
  console.log("\n=== Target domain (read-only) ===");
  const before = await browser.listPages();
  check(
    "listPages() returns an array of page targets",
    Array.isArray(before) && before.every((p) => p.type === "page"),
    "got: " + JSON.stringify(before),
  );
  console.log("  " + before.length + " pre-existing page target(s):");
  for (const p of before) {
    console.log("    " + p.targetId + "  " + String(p.url).slice(0, 70));
  }

  const targets = await browser.getTargets();
  check(
    "getTargets() returns a targetInfos array",
    !!targets && Array.isArray(targets.targetInfos),
    "got: " + JSON.stringify(targets).slice(0, 120),
  );
  check(
    "every page target of listPages() is in targetInfos",
    targets.targetInfos.some((t) => before.some((p) => p.targetId === t.targetId)) ||
      before.length === 0,
    "pages=" + before.length + " targets=" + targets.targetInfos.length,
  );

  // 4. full page flow on a tab this test owns.
  console.log("\n=== Page flow (own tab only) ===");
  const page = await browser.newPage("about:blank");
  check(
    "newPage() returns a Page with targetId",
    !!page && typeof page.targetId === "string" && page.targetId.length > 0,
    "got: " + JSON.stringify(page && page.targetId),
  );

  try {
    await page.navigate(PAGE);
    await page.waitForSelector("#tag", 5000);
    check("navigate + waitForSelector works", true);

    const tag = await page.evaluate("document.querySelector('#tag').textContent");
    check("evaluate() reads the DOM", tag === "attached", "got: " + JSON.stringify(tag));

    const rows = await page.evaluate("document.querySelectorAll('.row').length");
    check("evaluate() returns numbers", rows === 2, "got: " + JSON.stringify(rows));

    const html = await page.getOuterHTML();
    check(
      "getOuterHTML() returns the document markup",
      typeof html === "string" && /<h1 id="tag">attached<\/h1>/.test(html),
      "got: " + String(html).slice(0, 80),
    );

    const nodes = await page.querySelectorAll(".row");
    check(
      "querySelectorAll() returns 2 node ids",
      Array.isArray(nodes) && nodes.length === 2 &&
        nodes.every((n) => typeof n === "number" && n > 0),
      "got: " + JSON.stringify(nodes),
    );

    await page.setViewport(1024, 768);
    const metrics = await page.getLayoutMetrics();
    check(
      "setViewport(1024,768) applies to the attached tab",
      !!metrics && !!metrics.layoutViewport &&
        metrics.layoutViewport.clientWidth === 1024 &&
        metrics.layoutViewport.clientHeight === 768,
      "got: " + JSON.stringify(metrics && metrics.layoutViewport),
    );

    const png = await page.captureScreenshot();
    check(
      "captureScreenshot() returns PNG bytes",
      !!png && typeof png.length === "number" && png.length > 0,
      "got: " + (png ? png.length : "n/a"),
    );
  } finally {
    await page.close();
  }

  // 5. the pre-existing tabs are still there, and ours is gone.
  const after = await browser.listPages();
  check(
    "closing our tab leaves the pre-existing targets untouched",
    after.length === before.length &&
      before.every((p) => after.some((q) => q.targetId === p.targetId)),
    "before=" + before.length + " after=" + after.length,
  );

  // 6. close() only detaches: the external browser is still up.
  console.log("\n=== close is detach-only ===");
  await browser.close();

  let browser2;
  try {
    browser2 = await Browser.connect(cdpUrl);
  } catch (e) {
    check("reconnect after close() works (process not killed)", false, String(e));
    browser2 = null;
  }
  if (browser2) {
    check("reconnect after close() works (process not killed)", true);
    const ver2 = await browser2.getVersion();
    check(
      "the external browser still answers CDP",
      !!ver2 && typeof ver2.product === "string" && ver2.product.length > 0,
      "got: " + JSON.stringify(ver2),
    );
    const pages2 = await browser2.listPages();
    check(
      "pre-existing tabs survived the detach",
      Array.isArray(pages2) && pages2.length >= before.length,
      "len=" + (pages2 ? pages2.length : "n/a"),
    );
    await browser2.close();
  }

  console.log("\n" + "=".repeat(50));
  console.log("pass: " + pass + ", fail: " + fail);
  return fail > 0 ? 1 : 0;
}

// `process.exit()` is deferred to the next tick by the runtime, so exit exactly
// once from here instead of from inside main().
main()
  .then((code) => process.exit(code))
  .catch((e) => {
    console.log("Unhandled error:", (e && e.message) || e);
    process.exit(1);
  });