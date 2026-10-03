// Check `types/browser.d.ts` against the `#[zq_method]` registration table.
//
// The d.ts is maintained by hand, so the failure mode bugs.md recorded (B-036)
// is a method that exists in Rust but is missing here, or the reverse: both are
// invisible until a caller writes code against a declaration that lies. This
// diffs the two lists and exits non-zero on any mismatch.
//
//   aike -m tests/check_dts.mjs
//
// Reads `src/*.rs` for `#[zq_method(name = "x")]` / `#[zq_static_method(...)]`
// and `types/browser.d.ts` for interface member names. Members the native code
// installs by hand rather than through the macro are listed in HANDWRITTEN so
// they are not reported as missing.

import * as fs from "fs";

/** The two interfaces plus the process handle that carry registered methods. */
const CLASSES = ["Browser", "Page", "BrowserProcess"];

/** JS members the native code installs outside the `#[zq_method]` table. */
const HANDWRITTEN = {
  Browser: [],
  // `browser.newPage` installs it with `define_str`.
  Page: ["targetId"],
  BrowserProcess: [],
};

/** Rust impl block -> the d.ts interface its registrations belong to. */
const IMPL_CLASS = {
  JSBrowser: "Browser",
  JSPage: "Page",
  JSBrowserProcess: "BrowserProcess",
};

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

/** Every match of a global regex, as an array (no `matchAll` dependency). */
function all(re, text) {
  const out = [];
  re.lastIndex = 0;
  let m;
  while ((m = re.exec(text)) !== null) {
    out.push(m);
    if (m.index === re.lastIndex) re.lastIndex++;
  }
  return out;
}

function camel(snake) {
  return snake.replace(/_([a-z0-9])/g, function (_, c) {
    return c.toUpperCase();
  });
}

/** Read every `src/*.rs` and collect the JS name each registration installs. */
async function registeredNames() {
  const found = {};
  for (const c of CLASSES) found[c] = new Set();

  const files = await fs.glob("src/*.rs", { cwd: process.cwd(), dot: true });
  for (const file of files) {
    const text = await fs.readText(file);

    // Which class a registration belongs to: the nearest preceding impl block.
    const blocks = [];
    for (const b of all(/impl (JSBrowser|JSBrowserProcess|JSPage)\b/g, text)) {
      blocks.push({ index: b.index, cls: IMPL_CLASS[b[1]] });
    }

    for (const m of all(/#\[zq_(static_)?method(?:\(([^)]*)\))?\]/g, text)) {
      let cls = null;
      for (const b of blocks) {
        if (b.index < m.index) cls = b.cls;
      }
      if (!cls) continue;

      // `name = "x"` wins; otherwise derive the JS name from the fn below.
      const attrs = m[2] || "";
      const named = /name\s*=\s*"([^"]+)"/.exec(attrs);
      let key = named ? named[1] : null;
      if (!key) {
        const tail = text.slice(m.index, m.index + 400);
        const fn = /fn\s+([A-Za-z0-9_]+)/.exec(tail);
        if (fn) key = camel(fn[1]);
      }
      if (key) found[cls].add(key);
    }
  }
  return found;
}

/**
 * Declared member names of each interface, methods and properties alike.
 *
 * `Browser`'s statics live on `export class Browser` rather than on
 * `interface Browser`, so both blocks are read for it.
 */
async function declaredNames() {
  const text = await fs.readText("types/browser.d.ts");
  const out = {};
  for (const cls of CLASSES) {
    const names = new Set();
    for (const [start, member] of [
      ["interface " + cls + " {", /^\s{4}(?:readonly\s+)?([A-Za-z_$][\w$]*)\s*[(<:?]/gm],
      ["export class " + cls + " {", /^\s{4}static\s+([A-Za-z_$][\w$]*)\s*[(<:?]/gm],
    ]) {
      const at = text.indexOf(start);
      if (at < 0) continue;
      // The block ends at the first `}` in column 2 after its start.
      const end = text.indexOf("\n  }", at);
      for (const m of all(member, text.slice(at, end < 0 ? text.length : end))) {
        names.add(m[1]);
      }
    }
    out[cls] = names;
  }
  return out;
}

const registered = await registeredNames();
const declared = await declaredNames();

console.log("\n=== types/browser.d.ts vs the #[zq_method] registration table ===");
for (const cls of CLASSES) {
  const reg = registered[cls];
  const dec = declared[cls];
  for (const n of HANDWRITTEN[cls]) reg.add(n);

  const missing = [...reg].filter((n) => !dec.has(n)).sort();
  const extra = [...dec].filter((n) => !reg.has(n)).sort();
  const detail =
    missing.map((n) => "in Rust only: " + n).concat(extra.map((n) => "in d.ts only: " + n)).join("; ");
  check(
    cls + ": " + reg.size + " registered, " + dec.size + " declared",
    missing.length === 0 && extra.length === 0,
    detail,
  );
}

console.log("\n" + "=".repeat(50));
console.log("pass: " + pass + ", fail: " + fail);
if (fail > 0) process.exit(1);