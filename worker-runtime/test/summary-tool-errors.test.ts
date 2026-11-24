import { expect, it } from "bun:test";
import {
  errorEchoScrubber, redactRenderedDiagnostics, redactQuotedHarness,
  TOOL_ERROR_REDACTED, TOOL_ERROR_TEXT_OMITTED,
} from "../src/summary-tool-errors.ts";

const message = "page_missing_7q: no such wiki page at that path";
const harness = (...lines: string[]) => `<harness>\n${lines.join("\n")}\n</harness>`;

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

it("scrubs multiline diagnostics without dropping the surrounding tool output or warning", () => {
  const diagnostic = `${message}\n[stderr] archive index could not be read`;
  const errors = new Set<string>();
  const output = `12 rows\n${harness(`[error] ${diagnostic}`, "[warn] output shortened")}\nDone.`;
  expect(redactRenderedDiagnostics(output, errors)).toBe(
    `12 rows\n${harness(`[error] ${TOOL_ERROR_REDACTED}`, "[warn] output shortened")}\nDone.`,
  );
  expect(errorEchoScrubber(errors)(`Saw ${diagnostic} on retry.`)).toBe(
    `Saw ${TOOL_ERROR_TEXT_OMITTED} on retry.`,
  );
});

it("does not swallow a later valid result after an unclosed diagnostic", () => {
  const unclosed = `Rows read: 12\n<harness>\n[error] ${message.slice(0, 20)}`;
  expect(redactRenderedDiagnostics(unclosed, new Set())).toBe(unclosed);
  const output = `${unclosed}\n${harness(`[error] ${message}`)}`;
  expect(redactRenderedDiagnostics(output, new Set())).toBe(
    `${unclosed}\n${harness(`[error] ${TOOL_ERROR_REDACTED}`)}`,
  );
});

it.each([
  harness("[warn] output shortened"),
  harness(`[error] ${TOOL_ERROR_REDACTED}`),
])("leaves non-errors and an already redacted diagnostic unchanged: %p", (output) => {
  const errors = new Set<string>();
  expect(redactRenderedDiagnostics(output, errors)).toBe(output);
  expect(redactQuotedHarness(output, errors)).toBe(output);
  expect(errorEchoScrubber(errors)(output)).toBe(output);
});

it("leaves an oversized historical quote unchanged when it cannot be a serialized diagnostic", () => {
  const summary = `Board budget HK$18,450.\n<harness>[error] ${"z".repeat(2_100)}</harness>`;
  expect(redactQuotedHarness(summary, new Set())).toBe(summary);
});
