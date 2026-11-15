import { recordServices, SCREEN_READ_VERBS, type HostCommand } from "../src/features/workspace-apps/screen/screenServices";
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { ActionArgs, RecordCallVerb } from "../src/features/workspace-apps/screen/screenProtocol";

export {};
declare function require(name: string): any;

const assert = {
  equal(actual: unknown, expected: unknown, why = "values must match"): void {
    if (actual !== expected) throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  },
  ok(value: unknown, why = "condition must hold"): void {
    if (!value) throw new Error(why);
  },
};

const savedFetch = globalThis.fetch;

function makeReceipt(): MountReceipt {
  return {
    mount: `/api/app-screen/${"b".repeat(64)}`,
    bridge_nonce: "c".repeat(64),
    generation: 1,
    tag: "write-refusal-test",
    action_token: "a".repeat(64),
    tools: [],
    host_contract: "screen-records.v1",
    operations: [
      "records.list",
      "records.create",
      "records.update",
      "works.query",
      "works.create",
      "sends.prepare",
      "whatsapp.reply.prepare",
      "content.approve",
    ],
    navigation: null,
    install_id: "install-write-test",
    context_id: "context-write-test",
    digest: `sha256:${"d".repeat(64)}`,
    display_scope_key: "e".repeat(64),
    display_session_key: "f".repeat(64),
    remote_images: [],
  };
}

function makeFixture(responder: (path: string, body: Record<string, unknown>) => { status: number; json: unknown }) {
  let current = true;
  let refused = 0;
  const commands: HostCommand[] = [];
  const requests: { path: string; body: Record<string, unknown> }[] = [];

  globalThis.fetch = (async (url: RequestInfo | URL, init?: RequestInit) => {
    const rawUrl = String(url);
    const prefix = "/api/app-screen-services/";
    assert.ok(rawUrl.startsWith(prefix), `Unexpected URL ${rawUrl}`);
    const path = rawUrl.slice(prefix.length);
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
    requests.push({ path, body });
    const reply = responder(path, body);
    return new Response(JSON.stringify(reply.json), {
      status: reply.status,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  const services = recordServices(makeReceipt(), "write-refusal-domain", {
    current: () => current,
    refused: () => {
      refused += 1;
      current = false;
    },
    command: (cmd) => {
      commands.push(cmd);
    },
  });

  return {
    services,
    requests,
    commands,
    isCurrent: () => current,
    refusedCount: () => refused,
  };
}

function assertScreenReadVerbsParity(): void {
  const fs = require("fs");
  const path = require("path");
  const cwd = require("process").cwd();
  const candidates = [
    path.resolve(cwd, "crates/cadence-core/src/app_screen_decl.rs"),
    path.resolve(cwd, "..", "crates/cadence-core/src/app_screen_decl.rs"),
  ];
  const filePath = candidates.find((p: string) => fs.existsSync(p));
  assert.ok(filePath, "app_screen_decl.rs must be readable from test runner cwd");
  const src = fs.readFileSync(filePath, "utf8") as string;

  const enumStart = src.indexOf("pub enum ScreenOperation {");
  const enumEnd = src.indexOf("\n}\n", enumStart);
  assert.ok(enumStart >= 0 && enumEnd > enumStart, "locate ScreenOperation enum");
  const variantToVerb = new Map<string, string>();
  const enumBody = src.slice(enumStart, enumEnd);
  const renameRe = /#\[serde\(rename\s*=\s*"([^"]+)"\)\]\s+([A-Za-z0-9_]+),/g;
  let m: RegExpExecArray | null;
  while ((m = renameRe.exec(enumBody)) !== null) {
    variantToVerb.set(m[2], m[1]);
  }
  assert.ok(variantToVerb.size > 50, `parsed ${variantToVerb.size} ScreenOperation variants`);

  const fnStart = src.indexOf("pub fn is_screen_read(self) -> bool");
  const fnEnd = src.indexOf("\n    }", fnStart);
  assert.ok(fnStart >= 0 && fnEnd > fnStart, "locate is_screen_read()");
  const fnBody = src.slice(fnStart, fnEnd);
  const rustReadVerbs = new Set<string>();
  const selfRe = /Self::([A-Za-z0-9_]+)/g;
  while ((m = selfRe.exec(fnBody)) !== null) {
    const verb = variantToVerb.get(m[1]);
    assert.ok(verb, `known serde verb for Self::${m[1]}`);
    rustReadVerbs.add(verb as string);
  }

  const missingInTs = [...rustReadVerbs].filter((v) => !SCREEN_READ_VERBS.has(v));
  const extraInTs = [...SCREEN_READ_VERBS].filter((v) => !rustReadVerbs.has(v));
  assert.equal(JSON.stringify(missingInTs), "[]", "TS SCREEN_READ_VERBS must cover every Rust is_screen_read() verb");
  assert.equal(JSON.stringify(extraInTs), "[]", "TS SCREEN_READ_VERBS must not diverge from Rust is_screen_read()");
}

async function main(): Promise<void> {
  try {
    assertScreenReadVerbsParity();

    // 1. owner_required 403 on write keeps mount active; subsequent reads still succeed.
    {
      const fx = makeFixture((_path, body) => {
        if (body.operation === "records.create") {
          return { status: 403, json: { code: "owner_required", error: "Owner required." } };
        }
        return { status: 200, json: { rows: [{ id: "c1" }] } };
      });
      const createRes = await fx.services.call("records.create", { name: "Ada" } as ActionArgs);
      assert.equal(createRes.ok, false, "owner_required write returns refusal");
      if (!createRes.ok) assert.equal(createRes.refusal.code, "owner_required");
      assert.equal(fx.refusedCount(), 0, "owner_required 403 does not call host.refused()");
      assert.equal(fx.isCurrent(), true, "owner_required 403 keeps mount active");
      assert.equal((await fx.services.call("records.list", {})).ok, true, "subsequent read succeeds after owner_required 403");
    }

    // 2. Explicit native read-only 403 on non-read RECORD_CALL_VERBS (including Works, Sends, WhatsApp) keeps mount active.
    for (const verb of ["records.update", "works.create", "sends.prepare", "whatsapp.reply.prepare"] as const satisfies readonly RecordCallVerb[]) {
      const fx = makeFixture((_path, body) => {
        if (body.operation === verb) {
          return { status: 403, json: { code: "forbidden", error: "a read-only screen admits only read operations" } };
        }
        return { status: 200, json: { rows: [] } };
      });
      const res = await fx.services.call(verb, { id: "c1" } as ActionArgs);
      assert.equal(res.ok, false, `read-only 403 on ${verb} returns refusal`);
      if (!res.ok) {
        assert.equal(res.refusal.code, "forbidden", `code on ${verb}`);
        assert.equal(res.refusal.text, "This screen is read-only; write actions are not permitted.", `text on ${verb}`);
      }
      assert.equal(fx.refusedCount(), 0, `${verb} read-only 403 does not retire mount`);
      assert.equal((await fx.services.call("records.list", {})).ok, true, `reads still work after ${verb} 403`);
    }

    // 3. Hosted undeclared operation refusal (`operation_not_declared`) on non-read verb is non-fatal.
    {
      const fx = makeFixture((_path, body) => {
        if (body.operation === "records.create") {
          return { status: 403, json: { code: "operation_not_declared", error: "This app did not declare that screen operation." } };
        }
        return { status: 200, json: { rows: [] } };
      });
      const res = await fx.services.call("records.create", { name: "Ada" } as ActionArgs);
      assert.equal(res.ok, false, "operation_not_declared returns refusal");
      if (!res.ok) {
        assert.equal(res.refusal.code, "forbidden");
        assert.equal(res.refusal.text, "This operation is unavailable.");
      }
      assert.equal(fx.refusedCount(), 0, "operation_not_declared keeps mount active");
      assert.equal((await fx.services.call("records.list", {})).ok, true, "reads still work after operation_not_declared");
    }

    // 4. Slot verb 403 on slot-prepare (owner_required, check: read_only, check: member_role) keeps mount active.
    for (const [label, json, expectedCode] of [
      ["hosted owner_required", { code: "owner_required", error: "Only owners can approve." }, "owner_required"],
      ["native check read_only", { check: "read_only", error: "board is read-only — writes are disabled" }, "forbidden"],
      ["native check member_role", { check: "member_role", error: "action needs the board owner's role" }, "owner_required"],
    ] as const) {
      const fx = makeFixture((path) => {
        if (path === "slot-prepare") return { status: 403, json };
        return { status: 200, json: { rows: [] } };
      });
      const planRes = await fx.services.planner("content.approve", { content_id: "c-1" } as ActionArgs);
      assert.ok(!("run" in planRes), `slot-prepare (${label}) returns refusal object`);
      if (!("run" in planRes)) assert.equal(planRes.code, expectedCode, `code for ${label}`);
      assert.equal(fx.refusedCount(), 0, `slot-prepare (${label}) does not retire mount`);
      assert.equal((await fx.services.call("records.list", {})).ok, true, `mount alive after slot-prepare (${label})`);
    }

    // 5. Slot-confirm non-fatal check: read_only keeps mount active.
    {
      const fx = makeFixture((path) => {
        if (path === "slot-prepare") {
          return {
            status: 200,
            json: {
              confirmation_token: "a".repeat(64),
              card: { operation: "content.approve", facts: { campaign_id: "cmp-1", subject: "Hello", revision: 1 } },
            },
          };
        }
        if (path === "slot-confirm") {
          return { status: 403, json: { check: "read_only", error: "board is read-only — writes are disabled" } };
        }
        return { status: 200, json: { rows: [] } };
      });
      const plan = await fx.services.planner("content.approve", { content_id: "c-1" } as ActionArgs);
      assert.ok("run" in plan, "slot-prepare 200 produces runnable plan");
      if ("run" in plan) {
        const confirmRes = await plan.run();
        assert.equal(confirmRes.ok, false, "slot-confirm 403 returns refusal");
        assert.equal(fx.refusedCount(), 0, "slot-confirm check:read_only keeps mount active");
        assert.equal((await fx.services.call("records.list", {})).ok, true, "mount alive after slot-confirm 403");
      }
    }

    // 6. Fail-closed fatal 403/401 matrix: every session/origin/proof/revocation or unrecognized 403 retires mount.
    for (const [label, status, payload, mode, op] of [
      ["screen_access_denied 403 on write", 403, { code: "screen_access_denied", error: "Access revoked." }, "call", "records.create"],
      ["check board_session_required on write", 403, { check: "board_session_required", error: "sign in required" }, "call", "records.create"],
      ["check session_from_agent on write", 403, { check: "session_from_agent", error: "agent session refused" }, "call", "records.create"],
      ["check board_session_required on slot-prepare", 403, { check: "board_session_required", error: "sign in required" }, "slot-prepare", "content.approve"],
      ["check operator_proof on slot-confirm", 403, { check: "operator_proof", error: "operator proof failed" }, "slot-confirm", "content.approve"],
      ["live operator session message on write", 403, { code: "forbidden", error: "screen service requires live operator session" }, "call", "records.create"],
      ["unrecognized forbidden message on write fails closed", 403, { code: "forbidden", error: "Workspace member cannot edit." }, "call", "records.create"],
      ["read-only 403 on read verb still retires", 403, { code: "forbidden", error: "a read-only screen admits only read operations" }, "call", "records.list"],
      ["401 unauthorized on write", 401, { code: "unauthorized", error: "Session expired." }, "call", "records.create"],
      ["403 on read verb", 403, { code: "forbidden", error: "Forbidden." }, "call", "records.list"],
    ] as const) {
      const fx = makeFixture((path) => {
        if (mode === "slot-confirm" && path === "slot-prepare") {
          return {
            status: 200,
            json: {
              confirmation_token: "a".repeat(64),
              card: { operation: "content.approve", facts: { campaign_id: "cmp-1", subject: "Hello", revision: 1 } },
            },
          };
        }
        return { status, json: payload };
      });

      if (mode === "call") {
        const res = await fx.services.call(op as RecordCallVerb, {});
        assert.equal(res.ok, false, `${label}: call returns refusal`);
      } else if (mode === "slot-prepare") {
        const plan = await fx.services.planner(op, { content_id: "c-1" } as ActionArgs);
        assert.ok(!("run" in plan), `${label}: planner returns refusal`);
      } else {
        const plan = await fx.services.planner(op, { content_id: "c-1" } as ActionArgs);
        assert.ok("run" in plan, `${label}: prepare succeeded`);
        if ("run" in plan) {
          const confirmRes = await plan.run();
          assert.equal(confirmRes.ok, false, `${label}: confirm returns refusal`);
        }
      }

      const expectedRequests = mode === "slot-confirm" ? 2 : 1;
      assert.equal(fx.refusedCount(), 1, `${label}: calls host.refused() once`);
      const after = await fx.services.call("records.list", {});
      assert.equal(after.ok, false, `${label}: subsequent call blocked after retirement`);
      assert.equal(fx.requests.length, expectedRequests, `${label}: retired mount makes no extra fetch`);
    }

    console.log("screen write refusal checks passed");
  } finally {
    globalThis.fetch = savedFetch;
  }
}

void main();
