import { api, type ApiError } from "../src/lib/api";
import { QueryCache } from "../src/lib/cache";
import {
  approvalChip,
  gateBlock,
  missingRequired,
  proposeBlock,
  proposedEpic,
  providedInputs,
  refusalText,
  runFields,
} from "../src/features/projects/workflows";
import type { WorkflowRow } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const row = (over: Partial<WorkflowRow> = {}): WorkflowRow => ({
  project: "cadence",
  name: "two-step",
  title: "Change: title",
  tickets: 2,
  approved: true,
  inputs: [
    { name: "note", ask: null, optional: true },
    { name: "title", ask: "What change?", optional: false },
  ],
  ...over,
});

type Call = { url: string; init: RequestInit };

function stubFetch(status: number, body: unknown): Call[] {
  const calls: Call[] = [];
  (globalThis as { fetch: unknown }).fetch = async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return {
      ok: status >= 200 && status < 300,
      status,
      statusText: "",
      json: async () => body,
    } as Response;
  };
  return calls;
}

async function main() {
  // The run form's fields come from the workflow's frontmatter inputs —
  // `ask` is the label, `optional` marks the field; a missing ask falls
  // back to the input's name.
  {
    const fields = runFields(row());
    equal(
      fields.map((f) => [f.name, f.label, f.optional]),
      [
        ["note", "note", true],
        ["title", "What change?", false],
      ],
      "fields from frontmatter",
    );
    equal(runFields(row({ inputs: [] })), [], "no inputs → no fields");
    equal(runFields(row({ inputs: null })), [], "absent inputs → no fields");
    // CAD-571: a declared `example:` is the empty field's placeholder
    // ("the topic placeholder is an example"); else the input's name.
    equal(
      runFields(
        row({
          inputs: [
            { name: "topic", ask: "About what?", example: "How we onboard a new client" },
            { name: "slug", ask: "Folder name", kind: "slug" },
          ],
        }),
      ).map((f) => f.placeholder),
      ["How we onboard a new client", "slug"],
      "the example is the placeholder",
    );
  }

  // providedInputs sends declared names only, trimmed, non-empty — an
  // unknown key in the form state never reaches the wire (the daemon
  // refuses unknown inputs, so the client never produces one).
  {
    const inputs = providedInputs(row(), {
      title: "  login fix ",
      note: "",
      sneaky: "never sent",
    });
    equal(inputs, { title: "login fix" }, "declared, trimmed, non-empty");
    equal(
      providedInputs(row(), { title: "   ", note: "n" }),
      { note: "n" },
      "a whitespace-only value is absent",
    );
  }

  // missingRequired names the required fields still empty; optionals
  // never block.
  {
    equal(missingRequired(row(), {}), ["title"], "required missing");
    equal(missingRequired(row(), { title: "x" }), [], "required filled");
    equal(missingRequired(row({ inputs: [] }), {}), [], "no inputs → nothing missing");
  }

  // gateBlock: a broken file, or a gate approval that does not cover
  // the file's current keys — the daemon's own refusals, prefigured.
  {
    equal(gateBlock(row()), null, "approved → no gate block");
    equal(gateBlock(row({ error: "symlink" })), "symlink", "a broken file");
    const unapproved = gateBlock(row({ approved: false }))!;
    equal(unapproved.includes("not approved"), true, "unapproved says so");
    equal(
      unapproved.includes("cadence workflow approve two-step --project cadence"),
      true,
      "and names the fix",
    );
    equal(
      gateBlock(row({ approved: "unknown — daemon unreachable" })),
      "approval state unknown — daemon unreachable",
      "unreadable approvals",
    );
  }

  // proposeBlock in the order an operator meets the reasons: the
  // board's own write gate, the workflow's gate, the form's missing
  // inputs, then the board's operator proof.
  {
    const op = { readOnly: false, operator: true };
    equal(proposeBlock(row(), op, { title: "x" }), null, "a ready run");
    equal(
      proposeBlock(row(), { readOnly: true, operator: true }, { title: "x" }),
      "The board is read-only — sign in as the operator to propose a run.",
      "read-only first",
    );
    equal(
      proposeBlock(row({ approved: false }), op, { title: "x" })!.includes("not approved"),
      true,
      "the gate before inputs",
    );
    equal(
      proposeBlock(row(), op, {}),
      "missing required input: title",
      "missing inputs before the proof",
    );
    equal(
      proposeBlock(row(), { readOnly: false, operator: false }, { title: "x" })!.includes(
        "operator",
      ),
      true,
      "an unproven operator is told the CLI path",
    );
  }

  // approvalChip: the card's badge — broken, approved, unapproved, or
  // unreadable (a string the daemon could not answer).
  {
    equal(approvalChip(row()), { text: "approved", cls: "bg-ok/15 text-ok" }, "approved chip");
    equal(approvalChip(row({ error: "x" })).text, "broken", "broken chip");
    equal(approvalChip(row({ approved: false })).text, "unapproved", "unapproved chip");
    equal(
      approvalChip(row({ approved: "unknown — daemon unreachable" })).text,
      "approval unknown",
      "unknown chip",
    );
  }

  // proposedEpic lifts the plan the daemon answered with.
  {
    equal(
      proposedEpic({ epic: "CAD-4", title: "Change: x", tickets: ["CAD-5", "CAD-6"] }),
      { epic: "CAD-4", title: "Change: x", tickets: 2 },
      "proposed epic",
    );
    equal(proposedEpic({}), null, "no epic → null");
  }

  // refusalText names the daemon's refusal code beside its reason —
  // `one_line: input 'x' must be a single line…` — and reads as the
  // bare reason when the refusal carried no code.
  {
    equal(
      refusalText("not_distinct", "inputs 'a' and 'b' must differ"),
      "not_distinct: inputs 'a' and 'b' must differ",
      "named refusal",
    );
    equal(refusalText(null, "missing required input 'title'"), "missing required input 'title'", "no code");
    equal(refusalText("one_line", ""), "one_line: refused", "empty reason");
    equal(refusalText(undefined, undefined), "refused", "no reason at all");
  }

  // The API surface: list is a plain GET; preview encodes the inputs
  // map into `?inputs=` with encodeURIComponent (the server decodes
  // %XX, never '+'); propose posts {"inputs":{…}} and no more — the
  // board relays plan_propose, never a forged field.
  {
    let calls = stubFetch(200, { workflows: [] });
    await api.workflows("cadence");
    equal(calls[0].url, "/api/projects/cadence/workflows", "list url");

    calls = stubFetch(200, { rendered: "---\n" });
    await api.workflowPreview("cadence", "two-step", { title: "a b" });
    equal(
      calls[0].url,
      "/api/projects/cadence/workflows/two-step/preview?inputs=%7B%22title%22%3A%22a%20b%22%7D",
      "preview url — %20, not +",
    );

    calls = stubFetch(200, { epic: "CAD-4", tickets: ["CAD-5"] });
    const out = await api.workflowPropose("cadence", "two-step", { title: "a b" });
    equal(calls[0].url, "/api/projects/cadence/workflows/two-step/propose", "propose url");
    equal(calls[0].init.method, "POST", "propose POSTs");
    equal(
      JSON.parse(String(calls[0].init.body)),
      { inputs: { title: "a b" } },
      "propose body is inputs only",
    );
    equal(proposedEpic(out)?.epic, "CAD-4", "propose answer");

    // A daemon refusal keeps its wire code (workflow_unapproved etc).
    calls = stubFetch(400, { error: "workflow is not approved", code: "workflow_unapproved" });
    let err: ApiError | null = null;
    await api.workflowPropose("cadence", "two-step", {}).catch((e: ApiError) => {
      err = e;
    });
    const refusal = err as ApiError | null;
    equal([refusal?.status, refusal?.code], [400, "workflow_unapproved"], "daemon code crosses");
    equal(calls.length, 1, "sent once");
  }

  // The workflows cache family is keyed per project and invalidated as
  // one prefix — an "issues" stream frame's `workflows` entry reaches
  // every project's store.
  {
    const cache = new QueryCache();
    let fetches: string[] = [];
    const family = cache.family<string, string[]>("workflows", (p) => {
      fetches.push(p);
      return Promise.resolve([p]);
    });
    const a = family("alpha");
    const b = family("beta");
    a.subscribe(() => {});
    await a.refresh();
    await b.refresh();
    fetches = [];
    cache.invalidate("workflows");
    await new Promise((r) => setTimeout(r, 0));
    equal(fetches, ["alpha"], "only the observed store refetches");
    await b.revalidate();
    equal(fetches, ["alpha", "beta"], "the marked store refetches on view");
    fetches = [];
    cache.invalidate("issues");
    await new Promise((r) => setTimeout(r, 0));
    equal(fetches, [], "an unrelated frame fetches nothing");
  }

  console.log("workflows checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
