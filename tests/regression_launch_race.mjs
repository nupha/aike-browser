// Regression test: Browser.launch() must survive the startup port/WS race.
//
// Bug: `Browser.launch()` intermittently threw
//   CDP WebSocket connect failed: IO error: 由于目标计算机积极拒绝，无法连接。 (os error 10061)
// The endpoint probe (DevToolsActivePort) can observe the debug port a moment
// before Chrome's listener finishes binding, and the WS connect had no retry.
// Observed before the fix: 1/30 and 1/40 failures for page_input.mjs, 4/20 for
// page_dom.mjs in one direct-invocation batch.
//
// This test launches repeatedly and fails on ANY launch error, so the flake
// rate is directly observable. Run it several times; N cycles per run.
//
//   aike -m tests/regression_launch_race.mjs [cycles]

import { Browser } from "browser";

const cycles = Number(process.argv[2] || 25);

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

console.log("\n=== launch race: " + cycles + " consecutive launches ===");

const errors = [];
for (let i = 1; i <= cycles; i++) {
  let browser = null;
  try {
    browser = await Browser.launch({ headless: true });
    const page = await browser.newPage();
    await page.navigate("data:text/html,<h1>race</h1>");
    const title = await page.evaluate("document.querySelector('h1').textContent");
    if (title !== "race") throw new Error("page did not load: " + title);
    await page.close();
    await browser.close();
  } catch (e) {
    errors.push("cycle " + i + ": " + (e && (e.message || e)));
    try {
      if (browser) await browser.close();
    } catch (_) {}
  }
  if (i % 5 === 0) console.log("  ... " + i + "/" + cycles + " cycles, errors so far: " + errors.length);
}

check(
  "all " + cycles + " launches + navigations succeeded",
  errors.length === 0,
  errors.slice(0, 5).join(" | "),
);
check("no launch errors recorded", errors.length === 0, "count=" + errors.length);

console.log("\n" + "=".repeat(50));
console.log("pass: " + pass + ", fail: " + fail);
if (fail > 0) process.exit(1);
