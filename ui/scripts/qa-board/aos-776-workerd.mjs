#!/usr/bin/env node
// Optional local qualification, not CI. Run with Node >=22:
// node ui/scripts/qa-board/aos-776-workerd.mjs /absolute/path/to/agenticos-v2-aos-776
// Requires the pinned v2 checkout's installed dependencies; never contacts a deployed service.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { pathToFileURL } from "node:url";

const expected = "2c1bba077f5efc240b34db9a41c7b1a6ce201a67";
const root = process.argv[2];
if (!root || !isAbsolute(root) || process.argv.length !== 3) {
  throw new Error("Pass exactly one absolute path to the pinned agenticos-v2 checkout");
}
const git = (...args) => execFileSync("git", ["-C", root, ...args], { encoding: "utf8" }).trim();
const head = git("rev-parse", "HEAD");
assert.equal(head, expected, "Use the pinned AOS-776 synthetic source revision");
assert.equal(git("status", "--porcelain", "--untracked-files=no"), "", "v2 tracked source must be clean");
console.log(`AOS-776 synthetic source ${head}; shipped lifecycle source 8e1f78f0f58a6e10287c16a138087abc7d06401f`);

// Reuse the v2 owner's actual renderer, bundling, Miniflare bindings, D1
// migrations, placement, session signing and cleanup. Anchors fail closed if
// the pinned harness changes. Only the setup and assertions below are retained.
const base = await readFile(join(root, "tools/qa/tenant-board-smoke.mjs"), "utf8");
const at = (text) => {
  const index = base.indexOf(text);
  assert.notEqual(index, -1, `Missing v2 harness anchor: ${text}`);
  return index;
};
const replaceOnce = (source, before, after) => {
  assert.equal(source.split(before).length, 2, `Expected exactly one anchor: ${before}`);
  return source.replace(before, after);
};
const controlBundle = `  const controlBundle = await bundle(\`
    export { TenantBoardControl } from './src/tenant-board/control-entrypoint.ts';
    export { CompanyControl } from './src/company-control/control.ts';
    export default { fetch() { return new Response('Not found', {status:404}); } };
  \`, "control");
`;
const workerBundle = `  const workerBundle = await bundle(\`
    import worker, { TenantWorkspace as ActualWorkspace } from './src/tenant-board/worker.ts';
    import { TenantSessionV2 } from './src/tenant-board/session.ts';
    import { boardSessionHash } from './src/board/session.ts';
    import { stageScreenFixture } from './src/screen-package.test-support.ts';
    export { TenantSessionV2 };
    export class TenantWorkspace extends ActualWorkspace {
      privateFixture({action, digest, tokenHash}) {
        if (action === 'age') return this.setAppInstallationState({installId:'install_books',state:'removed',now:Date.now()-2592000001});
        if (action !== 'seed') throw new Error('unknown fixture action');
        const record = {installId:'install_books',name:'books',title:'Books',version:'1.0.0',summary:'',digest,approved:true,screens:['main'],sourceKind:'private',smtpCustody:false,createdAt:Date.now(),updatedAt:Date.now()};
        const storage = {contract:'app-storage/v1',app:'books',recordKind:'person',recordWriteSchema:null,schemaVersion:1,compatibleSchemaVersions:[1],defaultTable:'people',migrations:[{version:1,path:'storage/001-initial.json',sha256:'sha256:'+'1'.repeat(64),operations:[{op:'create_table',table:'people',columns:[{name:'name',type:'text',nullable:false,maxLength:40}]}]}]};
        return this.applyAppInstallation({record,storage,expectedDigest:null,authority:{kind:'board-session',tokenHash}});
      }
    }
    export default { async fetch(request, env, ctx) {
      if (new URL(request.url).pathname === '/__smoke/private-fixture') {
        const input = await request.json();
        if (input.action !== 'seed') return Response.json(await env.TENANT_WORKSPACE.getByName(env.TENANT_WORKSPACE_ID).privateFixture(input));
        const staged = await stageScreenFixture('books', {host_contract:'screen-records.v1',may:['records.call','records.slot'],tools:{},operations:['records.list','records.create','records.show']});
        for (const [key, bytes] of staged.bucket.objects) await env.BOARD_ASSETS.put(key, bytes);
        const result = await env.TENANT_WORKSPACE.getByName(env.TENANT_WORKSPACE_ID).privateFixture({action:'seed',digest:staged.digest,tokenHash:await boardSessionHash(input.token)});
        return Response.json({...result,digest:staged.digest});
      }
      return worker.fetch(request, env, ctx);
    } };
  \`, "tenant");
`;
const cases = `
  const [a] = tenants;
  const json = async (response, status) => {
    const text = await response.text();
    assert.equal(response.status, status, text);
    return text ? JSON.parse(text) : null;
  };
  await control.prepare("INSERT INTO user(id,name,email,email_verified,created_at,updated_at) VALUES(?,?,?,?,?,?)")
    .bind("member", "Synthetic member", "member@example.test", 1, Date.now(), Date.now()).run();
  await control.prepare("INSERT INTO workspace_members(workspace_id,user_id,role,created_at) VALUES(?,?,?,?)")
    .bind(a.workspaceId, "member", "member", Date.now()).run();
  await (await mf.getR2Bucket("BOARD_ASSETS", a.slug)).put("_tenant/identity.json", artifact(a.slug + "/_tenant/identity.json"));
  const owner = await mint(a, await sign(a));
  const member = await mint(a, await sign(a, "member", "member"));
  assert.equal(owner.status, 200, await owner.clone().text());
  assert.equal(member.status, 200, await member.clone().text());
  const ownerCookie = owner.headers.get("set-cookie")?.split(";")[0];
  const memberCookie = member.headers.get("set-cookie")?.split(";")[0];
  assert.ok(ownerCookie); assert.ok(memberCookie);
  cookies.set(a.slug, ownerCookie);
  const fixture = (data) => request(a, "/__smoke/private-fixture", {
    method:"POST", headers:{"content-type":"application/json"}, body:JSON.stringify(data),
  });
  const seeded = await json(await fixture({action:"seed",token:ownerCookie.slice(ownerCookie.indexOf("=")+1)}), 200);
  assert.equal(seeded.ok, true);
  const path = "/api/app-installations/install_books";
  const readInstall = () => get(a,path).then(response => json(response,200));
  const post = (url, cookie = ownerCookie, origin = "https://"+a.host, payload) => request(a,url,{
    method:"POST",headers:{cookie,origin,"x-cadence-board":"1",...(payload ? {"content-type":"application/json"} : {})},
    ...(payload ? {body:JSON.stringify(payload)} : {}),
  });
  const mount = () => post(path+"/screens/main/mount",ownerCookie,undefined,{});
  const call = (token,operation,args) => post("/api/app-screen-services/call",ownerCookie,undefined,{action_token:token,operation,args});
  assert.equal((await json(await get(a,"/api/meta"),200)).app_private_removal,true);
  assert.equal((await json(await get(a,"/api/meta",memberCookie),200)).app_private_removal,undefined);
  const before = await json(await mount(),201);
  await json(await call(before.action_token,"records.create",{record_id:"keep",profile:{name:"Kept"}}),200);
  assert.equal((await post(path+"/remove",memberCookie)).status,403);
  assert.equal((await post(path+"/remove",ownerCookie,"https://other.example.test")).status,403);
  assert.equal((await readInstall()).state,"active");
  const removed = await json(await post(path+"/remove"),200);
  assert.equal(removed.install_id,"install_books"); assert.equal(removed.source_kind,"private");
  assert.equal(removed.state,"removed"); assert.equal(removed.executable,false);
  assert.equal(removed.restore_until-removed.removed_at,2592000000);
  assert.equal((await mount()).status,404);
  assert.equal((await call(before.action_token,"records.show",{record_id:"keep"})).status,403);
  assert.equal((await post(path+"/restore",memberCookie)).status,403);
  assert.equal((await post(path+"/restore",ownerCookie,"https://other.example.test")).status,403);
  assert.equal((await readInstall()).state,"removed");
  const restored = await json(await post(path+"/restore"),200);
  assert.equal(restored.restored,true); assert.equal(restored.installation.install_id,removed.install_id);
  assert.equal(restored.installation.digest,seeded.digest); assert.equal(restored.installation.state,"active");
  const after = await json(await mount(),201);
  assert.equal((await json(await call(after.action_token,"records.show",{record_id:"keep"}),200)).record.profile.name,"Kept");
  await json(await fixture({action:"age"}),200);
  assert.equal((await json(await post(path+"/restore"),409)).error.code,"restore_window");
  assert.equal((await readInstall()).state,"removed");
  console.log("PASS real workerd: owner same-ID/digest restore retained record; member and cross-origin denied; expired restore refused");
`;
let rig = base.slice(0, at("try {\n  const controlBundle")) + "try {\n" + controlBundle + workerBundle +
  base.slice(at("  const options = {"), at('  for (const mode of ["bigint"')) +
  base.slice(at('  let control = await mf.getD1Database'), at('  const ownControl = await request')) +
  cases + base.slice(base.lastIndexOf('} finally {'));
rig = replaceOnce(rig, '["alpha", "beta"]', '["alpha"]');
rig = replaceOnce(rig, '            SMOKE_METADATA: { className: "MetadataSession", useSQLite: true },\n', '');
rig = replaceOnce(rig, 'const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");', `const root = ${JSON.stringify(root)};`);
rig = replaceOnce(rig, 'import { wasmModules } from "../tenant-board/upload.mjs";', 'import { wasmModules, pdfAttachmentAssets } from "../tenant-board/upload.mjs";');
rig = replaceOnce(rig, 'plugins: [wasmModules(wasm)]', 'plugins: [wasmModules(wasm), pdfAttachmentAssets(require)]');
rig = rig.replaceAll('from "../tenant-board/', `from "${pathToFileURL(join(root, 'tools/tenant-board')).href}/`)
  .replaceAll('from "../../packages/', `from "${pathToFileURL(join(root, 'packages')).href}/`);
const scratch = await mkdtemp(join(tmpdir(), "aos-776-runner-"));
try {
  const file = join(scratch, "qualification.mjs");
  await writeFile(file, rig);
  const child = spawnSync(process.execPath, ["--experimental-transform-types", file], { stdio: "inherit" });
  if (child.error) throw child.error;
  if (child.status !== 0) throw new Error(`Qualification failed: ${child.signal ?? child.status}`);
} finally {
  await rm(scratch, { recursive: true, force: true });
}
