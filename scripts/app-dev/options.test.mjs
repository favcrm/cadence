import assert from "node:assert/strict";
import { test } from "node:test";
import { devOptions } from "./options.mjs";
test("dev preview refuses production and ambiguous API configuration", () => {
  for (const args of [
    ["social-content", "--port", "3010"],
    ["social-content", "--backend", "http://127.0.0.1:3010"],
    ["social-content", "--backend", "http://127.0.0.1:3110"],
    ["social-content", "--token", "secret"],
    ["../social-content"],
    ["social-content", "--host", "example.com"],
    ["social-content", "--allow-host", "*"],
  ]) {
    assert.throws(() => devOptions(args), undefined, JSON.stringify(args));
  }
});
test("fixtures are default and only explicitly private hostnames are allowed", () => {
  assert.deepEqual(devOptions(["social-content"]), {
    app: "social-content",
    port: 3186,
    host: "127.0.0.1",
    allowedHosts: [],
  });
  assert.equal(devOptions(["social-content", "--port", "3187"]).port, 3187);
});
