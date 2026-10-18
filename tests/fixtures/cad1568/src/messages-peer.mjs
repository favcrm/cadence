// Controlled CAD-1568 peer: actual AgenticOS app/D1 and existing native
// forwarder. Only account/consent setup and post-commit response loss are
// synthetic; no production authorization handler or response is replaced.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const enabled = process.env.CAD1568_NATIVE_CHAIN === "1";
const audience = "https://lc.cadencecloud.app";
const org = "lc_ws";
const caps = ["cli.read", "cli.write", "wiki.read", "wiki.write"];
const verifier = "b".repeat(43);

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`CAD-1568 peer needs ${name}`);
  return value;
}

describe.skipIf(!enabled)("CAD-1568 real hosted messaging authority", () => {
  let ctx;
  let bridge;
  let stopBridge;
  afterAll(async () => {
    if (bridge) await stopBridge(bridge);
    await ctx?.dispose();
  });

  it("serves real device grants and native calls until controller completion", async () => {
    const api = required("CAD1568_AOS_API_DIR");
    // The API checkout is runtime-selected by the controller. Static imports
    // cannot name another repository's checkout without hardcoding host paths.
    const { createTestContext, requestOtp, signInWithOtp, sessionCookie, postJson } =
      await import(`${api}/src/testkit.ts`);
    const { nativeForwarder } = await import(`${api}/src/hosted-cadence/native-forwarder.test-support.ts`);
    const { startTlsBridge, stopTlsBridge } = await import(`${api}/src/hosted-cadence/tls-bridge.test-support.ts`);
    stopBridge = stopTlsBridge;
    const port = Number(required("CAD1434_BRIDGE_PORT"));
    const issuer = `https://issuer.board.localhost:${port}`;
    const output = required("CAD1434_TOKEN_OUT");
    const runtime = nativeForwarder({
      nativeOrigin: required("CAD1434_NATIVE_ORIGIN"),
      seamToken: readFileSync(join(dirname(output), "native-seam"), "utf8"),
    });
    ctx = await createTestContext({
      BOARD_DOMAIN: "cadencecloud.app",
      BOARD_ASSERTION_KEYS: required("CAD1434_BOARD_KEYS"),
      AGENTICOS_BOARD_ISSUER: issuer,
      HOSTED_CADENCE_GATEWAY_ENABLED: "true",
      CREDENTIALS_KEK: JSON.stringify({
        current: "cad1568",
        keys: { cad1568: btoa(String.fromCharCode(...new Uint8Array(32).fill(68))) },
      }),
    }, { boardRuntime: () => runtime });

    const people = {};
    for (const role of ["owner", "member"]) {
      const email = `cad1568-${role}@example.test`;
      expect((await requestOtp(ctx, email)).status).toBe(200);
      const signed = await signInWithOtp(ctx, email, await ctx.latestOtp(email));
      expect(signed.status).toBe(200);
      people[role] = { cookie: sessionCookie(signed), id: (await signed.json()).user.id };
    }
    const at = Date.now();
    await ctx.db.prepare("INSERT INTO workspaces(id,name,slug,created_at,updated_at) VALUES(?,?,?,?,?)")
      .bind(org, "CAD-1568", "lc", at, at).run();
    for (const role of ["owner", "member"]) {
      await ctx.db.prepare("INSERT INTO workspace_members(workspace_id,user_id,role,created_at) VALUES(?,?,?,?)")
        .bind(org, people[role].id, role, at).run();
    }
    ctx.env.BETTER_AUTH_URL = "https://api.example.test";
    // Capture OTP only at testkit's local origin; switch to a canonical
    // portless API origin only after synthetic users have signed in.
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
    const challenge = btoa(String.fromCharCode(...new Uint8Array(digest)))
      .replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
    const grants = {};
    for (const role of ["owner", "member"]) {
      const issued = await postJson(ctx, "/v1/hosted-cadence/device/code", {
        version: "hosted-cadence-device.v1", client_label: `CAD-1568 ${role}`,
        requested_capabilities: caps, code_challenge: challenge, cli_session: "renewable",
      });
      expect(issued.status).toBe(200);
      const code = await issued.json();
      const approved = await postJson(ctx, "/v2/hosted-cadence/device/approve", {
        user_code: code.user_code, organization_id: org, audience, capabilities: caps,
      }, { cookie: people[role].cookie });
      expect(approved.status).toBe(200);
      const token = await ctx.app.fetch(new Request(`${issuer}/v1/hosted-cadence/device/token`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ device_code: code.device_code, code_verifier: verifier }),
      }), ctx.env);
      expect(token.status).toBe(200);
      grants[role] = await token.json();
      expect(grants[role].principal.current_role).toBe(role);
      expect(grants[role].organization_slug).toBe("lc");
    }

    let dropped = false;
    let cliCalls = 0;
    const { promise: finished, resolve: finish } = Promise.withResolvers();
    bridge = await startTlsBridge({
      files: { certPath: required("CAD1434_TLS_CERT"), keyPath: required("CAD1434_TLS_KEY") },
      port, env: ctx.env, boardOrigin: audience, issuerOrigin: issuer,
      controlNonce: required("CAD1434_CONTROL_NONCE"),
      app: {
        async fetch(request, env) {
          const path = new URL(request.url).pathname;
          const input = path === "/__platform/cli/call" ? await request.clone().json() : undefined;
          const response = await ctx.app.fetch(request, env);
          if (path === "/__platform/cli/call") cliCalls++;
          if (!dropped && input?.verb === "message_send" &&
              input.arguments?.message === "cad1568-lost-receipt") {
            expect(response.status).toBe(200); // native handler already committed
            dropped = true;
            return new Response("acceptance receipt deliberately lost after commit", { status: 502 });
          }
          return response;
        },
      },
      observe() {},
      async control(op) {
        if (op === "revoke_owner_session") {
          // Authoritative membership removal with the bearer and signed
          // envelope retained: /call must re-check current membership.
          await ctx.db.prepare("DELETE FROM workspace_members WHERE workspace_id=? AND user_id=?")
            .bind(org, people.owner.id).run();
        } else if (op === "finish") {
          expect(dropped).toBe(true);
          expect(cliCalls).toBeGreaterThan(0);
          setImmediate(finish);
        } else {
          throw new Error("unsupported CAD-1568 fixture control");
        }
      },
    });
    writeFileSync(output, JSON.stringify({
      owner: grants.owner, member: grants.member,
      owner_id: people.owner.id, member_id: people.member.id, issuer, audience,
    }), { mode: 0o600, flag: "wx" });
    // Vitest's integration-test deadline bounds this event-based wait.
    await finished;
  }, 300_000);
});
