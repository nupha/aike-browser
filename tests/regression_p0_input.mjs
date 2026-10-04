// Regression test: the P0 Input-domain and uploadFile fixes.
//
//   B-007 `keyPress` wrote the character into both `keyDown` and a `char`
//         event, so every printable key was typed twice.
//   B-008 `keyPress("Enter")` (any named key) sent `text: "Enter"`, which Chrome
//         rejects with `Invalid 'text' parameter`.
//   B-009 the virtual key code was the first character's code point, so
//         `keyPress("Enter")` claimed `windowsVirtualKeyCode: 69`.
//   B-018 `uploadFile` built `new File([new Blob()], path)` in page script:
//         0-byte files named after the whole absolute path.
//
//   aike -m tests/regression_p0_input.mjs

import { Browser } from "browser";

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

// Poll without setTimeout: every awaited CDP round-trip gives the event loop a
// tick, which is what lets a dispatched key event land in the page.
// A stalled CDP round-trip means "not ready", not "broken": Chrome occasionally
// leaves `Runtime.evaluate` unanswered for its full timeout while a navigation
// is settling. Keep polling instead of failing on it.
async function poll(page, fn, tries = 80) {
  let v;
  for (let i = 0; i < tries; i++) {
    try {
      v = await fn();
    } catch (_) {
      v = null;
    }
    if (v) return v;
    // Another round-trip is another event-loop tick for the native event job.
    try {
      await page.evaluate("0");
    } catch (_) {}
  }
  return v;
}

// `file:///dir/file.mjs` -> `/dir/file.mjs`, `file:///C:/dir/f.mjs` -> `C:/dir/f.mjs`.
// The separators stay `/` on every platform: forcing `\` here handed Chrome a
// path that does not exist on unix, and a missing file is accepted silently as
// a 0-byte File named after the whole path -- exactly the B-018 symptom.
function fileURLToPath(url) {
  let p = String(url);
  const scheme = "file://";
  if (p.indexOf(scheme) === 0) p = p.slice(scheme.length);
  // Strip the (empty on unix, a drive name on Windows) authority component.
  if (p.charAt(0) === "/" && /^\/[A-Za-z]:/.test(p)) p = p.slice(1);
  return decodeURIComponent(p);
}

const INPUT_PAGE =
  "data:text/html," +
  encodeURIComponent(
    "<!DOCTYPE html><html><body>" +
      '<form id="f"><input id="k"></form>' +
      '<p id="out"></p>' +
      "<script>" +
      "document.getElementById('f').addEventListener('submit', function (e) {" +
      "  e.preventDefault();" +
      "  document.getElementById('out').textContent = 'submitted:' + document.getElementById('k').value;" +
      "});" +
      "document.getElementById('k').addEventListener('keydown', function (e) {" +
      "  window.__keys = (window.__keys || []).concat([e.key + '/' + e.keyCode]);" +
      "});" +
      "</script></body></html>",
  );

// B-007: a printable key inserts exactly one character.
async function testSingleCharacter(page) {
  console.log("\n=== B-007: keyPress inserts one character ===");
  await page.navigate(INPUT_PAGE);
  await page.waitForSelector("#k", 5000);
  await page.evaluate("document.getElementById('k').value = ''; window.__keys = [];");
  await page.focus("#k");

  await page.keyPress("a");
  const value = await poll(page, async () => {
    const v = await page.evaluate("document.getElementById('k').value");
    return v && v.length ? v : "";
  });
  check("keyPress('a') types exactly one character", value === "a", "value=" + JSON.stringify(value));

  await page.evaluate("document.getElementById('k').value = ''; window.__keys = [];");
  await page.focus("#k");
  await page.keyPress("Z");
  const value2 = await poll(page, async () => {
    const v = await page.evaluate("document.getElementById('k').value");
    return v && v.length ? v : "";
  });
  check("keyPress('Z') types exactly one character", value2 === "Z", "value=" + JSON.stringify(value2));
}

// B-008 / B-009: named keys resolve to a real key event with the right
// virtual key code, and the browser does not reject them.
async function testNamedKeys(page) {
  console.log("\n=== B-008/B-009: named keys ===");
  await page.navigate(INPUT_PAGE);
  await page.waitForSelector("#k", 5000);
  await page.evaluate("document.getElementById('k').value = ''; window.__keys = [];");
  await page.focus("#k");

  let enterError = null;
  try {
    await page.keyPress("Enter");
  } catch (e) {
    enterError = e && (e.message || e);
  }
  check("keyPress('Enter') resolves", enterError === null, "error=" + enterError);

  const keys = await poll(page, async () => {
    const v = await page.evaluate("window.__keys ? window.__keys.join(',') : ''");
    return v && v.length ? v : "";
  });
  check("keyPress('Enter') delivers key 'Enter'", keys.indexOf("Enter") === 0, "keys=" + keys);
  check("keyPress('Enter') reports virtual key code 13", keys.indexOf("Enter/13") === 0, "keys=" + keys);
  check(
    "Enter submits the form (default action still runs)",
    await poll(page, async () => {
      const v = await page.evaluate("document.getElementById('out').textContent");
      return v && v.indexOf("submitted:") === 0 ? v : "";
    }) !== "",
  );

  // Other named keys must not be rejected either.
  for (const key of ["Tab", "Escape", "Backspace", "ArrowLeft", "F5"]) {
    let err = null;
    try {
      await page.keyPress(key);
    } catch (e) {
      err = e && (e.message || e);
    }
    check("keyPress('" + key + "') resolves", err === null, "error=" + err);
  }

  // An unknown multi-character key is a caller error, not a silent protocol
  // failure later on.
  let unknown = null;
  try {
    await page.keyPress("NoSuchKey");
  } catch (e) {
    unknown = e && (e.message || e);
  }
  check("keyPress('NoSuchKey') throws", typeof unknown === "string", "error=" + unknown);
}

// B-018: the input receives the real file, not an empty one named after the
// whole path. The fixture is this very test file, so it is guaranteed to exist
// and to be non-empty without needing a filesystem API.
async function testUploadFile(browser) {
  console.log("\n=== B-018: uploadFile attaches the real file ===");
  const page = await browser.newPage();
  await page.navigate("data:text/html,<input id='f' type='file' multiple>");
  await page.waitForSelector("#f", 5000);

  const path = fileURLToPath(import.meta.url);
  const base = path.split("/").pop();

  let error = null;
  try {
    await page.uploadFile("#f", [path]);
  } catch (e) {
    error = e && (e.message || e);
  }
  check("uploadFile resolves", error === null, "error=" + error);

  const info = await page.evaluate(
    "(function () {" +
      "  var f = document.getElementById('f').files[0];" +
      "  return f ? { name: f.name, size: f.size } : null;" +
      "})()",
  );
  check("file input received exactly one file", info !== null, "info=" + JSON.stringify(info));
  check(
    "file name is the base name, not the whole path",
    info !== null && info.name === base,
    "name=" + (info && info.name) + " expected=" + base,
  );
  check(
    "file has the fixture's bytes, not an empty Blob",
    info !== null && info.size > 0,
    "size=" + (info && info.size),
  );
  await page.close();
}

async function main() {
  const browser = await Browser.launch({ headless: true });
  check("Browser launched", browser.isRunning === true);
  const page = await browser.newPage();

  const tests = [testSingleCharacter, testNamedKeys];
  for (const fn of tests) {
    try {
      await fn(page);
    } catch (e) {
      console.log("  [FAIL] " + fn.name + " threw: " + (e && (e.message || e)));
      fail++;
    }
  }
  try { await page.close(); } catch (_) {}

  try {
    await testUploadFile(browser);
  } catch (e) {
    console.log("  [FAIL] testUploadFile threw: " + (e && (e.message || e)));
    fail++;
  }

  try { await browser.close(); } catch (_) {}

  console.log("\n" + "=".repeat(50));
  console.log("pass: " + pass + ", fail: " + fail);
  if (fail > 0) throw new Error("regression_p0_input: " + fail + " check(s) failed");
}

await main();