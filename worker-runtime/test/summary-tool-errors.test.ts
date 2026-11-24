import { expect, it } from "bun:test";
import {
  errorEchoScrubber, redactQuotedHarness, TOOL_ERROR_REDACTED,
} from "../src/summary-tool-errors.ts";

const message = "page_missing_7q: no such wiki page at that path";

it("scrubs an earlier summary's diagnostic quote and echoed message, keeping warnings and business facts", () => {
  const errors = new Set<string>();
  const summary = redactQuotedHarness(
    `Result: <harness>\n[warn] output shortened\n[error] ${message}\n</harness>\n\nError: ${message}\n\nTill error E-4417 blocks launch.`,
    errors,
  );
  expect(errorEchoScrubber(errors)(summary)).toBe(
    `Result: <harness>\n[warn] output shortened\n[error] ${TOOL_ERROR_REDACTED}\n</harness>\n\nError: (tool error text omitted)\n\nTill error E-4417 blocks launch.`,
  );
});

it("keeps facts after an unclosed block and still scrubs a later one-line quote", () => {
  const summary = `Tried <harness>[error] cut off\nBudget HK$18,450.\nQuote: \`<harness>[error] ${message}</harness>\`.`;
  expect(redactQuotedHarness(summary, new Set())).toBe(
    `Tried <harness>[error] cut off\nBudget HK$18,450.\nQuote: \`<harness>[error] ${TOOL_ERROR_REDACTED}</harness>\`.`,
  );
});

it("does not delete a common short word or another identifier when scrubbing error echoes", () => {
  const scrub = errorEchoScrubber(new Set(["timeout", message]));
  expect(scrub("Prior timeout; page_missing_7q and my_page_missing_7q.")).toBe(
    "Prior timeout; (tool error code omitted) and my_page_missing_7q.",
  );
});

it("bounds scanning of repeated unclosed quote markers", () => {
  const summary = "<harness>[error] y ".repeat(16_000);
  const started = performance.now();
  expect(redactQuotedHarness(summary, new Set())).toBe(summary);
  expect(performance.now() - started).toBeLessThan(500);
});
