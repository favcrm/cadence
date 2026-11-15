import { recordServices, type HostCommand } from "../src/features/workspace-apps/screen/screenServices";
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { ActionArgs } from "../src/features/workspace-apps/screen/screenProtocol";

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
    operations: ["records.list", "records.create", "records.update", "content.approve"],
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

async function main(): Promise<void> {
  try {
    // 1. owner_required 403 on write keeps mount active; subsequent reads still succeed.
    {
      let seq = 0;
      const fx = makeFixture((_path, body) => {
        seq += 1;
        if (body.operation === "records.create") {
          return { status: 403, json: { code: "owner_required", error: "Owner required." } };
        }
        return { status: 200, json: { rows: [{ id: "c1" }], seq } };
      });
      const createRes = await fx.services.call("records.create", { name: "Ada" } as ActionArgs);
      assert.equal(createRes.ok, false, "owner_required write returns refusal");
      if (!createRes.ok) assert.equal(createRes.refusal.code, "owner_required");
      assert.equal(fx.refusedCount(), 0, "owner_required 403 does not call host.refused()");
      assert.equal(fx.isCurrent(), true, "owner_required 403 keeps mount active");

      const listRes = await fx.services.call("records.list", {});
      assert.equal(listRes.ok, true, "subsequent read still succeeds after non-fatal 403");
    }

    // 2. Read-only 403 (plain forbidden) on write verb keeps mount active.
    {
      const fx = makeFixture((_path, body) => {
        if (body.operation === "records.update") {
          return { status: 403, json: { code: "forbidden", error: "Workspace member cannot edit." } };
        }
        return { status: 200, json: { rows: [] } };
      });
      const updRes = await fx.services.call("records.update", { id: "c1" } as ActionArgs);
      assert.equal(updRes.ok, false, "read-only write returns refusal");
      if (!updRes.ok) {
        assert.equal(updRes.refusal.code, "forbidden");
        assert.equal(updRes.refusal.text, "This screen is read-only; write actions are not permitted.");
      }
      assert.equal(fx.refusedCount(), 0, "write 403 does not call host.refused()");
      const listRes = await fx.services.call("records.list", {});
      assert.equal(listRes.ok, true, "reads continue to work after read-only write 403");
    }

    // 3. Slot verb 403 on planner (slot-prepare and slot-confirm) keeps mount active.
    {
      const fx = makeFixture((path) => {
        if (path === "slot-prepare") {
          return { status: 403, json: { code: "owner_required", error: "Only owners can approve." } };
        }
        return { status: 200, json: { rows: [] } };
      });
      const planRes = await fx.services.planner("content.approve", { content_id: "c-1" } as ActionArgs);
      assert.ok(!("run" in planRes), "slot-prepare 403 returns refusal object");
      if (!("run" in planRes)) assert.equal(planRes.code, "owner_required");
      assert.equal(fx.refusedCount(), 0, "slot-prepare 403 does not retire mount");
      assert.equal((await fx.services.call("records.list", {})).ok, true, "mount alive after slot-prepare 403");
    }

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
          return { status: 403, json: { code: "forbidden", error: "Read-only role." } };
        }
        return { status: 200, json: { rows: [] } };
      });
      const plan = await fx.services.planner("content.approve", { content_id: "c-1" } as ActionArgs);
      assert.ok("run" in plan, "slot-prepare 200 produces runnable plan");
      if ("run" in plan) {
        const confirmRes = await plan.run();
        assert.equal(confirmRes.ok, false, "slot-confirm 403 returns refusal");
        assert.equal(fx.refusedCount(), 0, "slot-confirm 403 keeps mount active");
        assert.equal((await fx.services.call("records.list", {})).ok, true, "mount alive after slot-confirm 403");
      }
    }

    // 4. Fatal mount-revoked 403, 401, and read-verb 403 MUST retire mount and call host.refused().
    for (const [label, status, payload, op] of [
      ["screen_access_denied 403 on write", 403, { code: "screen_access_denied", error: "Access revoked." }, "records.create"],
      ["401 unauthorized on write", 401, { code: "unauthorized", error: "Session expired." }, "records.create"],
      ["403 on read verb", 403, { code: "forbidden", error: "Forbidden." }, "records.list"],
    ] as const) {
      const fx = makeFixture(() => ({ status, json: payload }));
      const res = await fx.services.call(op, {});
      assert.equal(res.ok, false, `${label}: call returns refusal`);
      assert.equal(fx.refusedCount(), 1, `${label}: calls host.refused() once`);
      const after = await fx.services.call("records.list", {});
      assert.equal(after.ok, false, `${label}: subsequent call blocked after retirement`);
      assert.equal(fx.requests.length, 1, `${label}: retired mount makes no second fetch`);
    }

    console.log("screen write refusal checks passed");
  } finally {
    globalThis.fetch = savedFetch;
  }
}

void main();
