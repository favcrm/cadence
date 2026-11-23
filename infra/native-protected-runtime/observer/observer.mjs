// CAD-1085 pinned Pi 1.4.0 observer.
//
// Loaded by the measured Node graph (`-e /opt/cadence/node/observer.mjs`) and
// run as the guest's PID 1 extension. It does exactly three things:
//
//   1. apply + verify the post-exec seal through the reviewed native addon
//      (`/opt/cadence/node/cadence_seal.node`) at extension install, BEFORE
//      asking for the observation key: non-dumpable, core limit 0,
//      no_new_privs, kernel-visible non-dumpable /proc proof. Every
//      observation re-proves the same facts;
//   2. compute the ACTUAL active tool registry from Pi's own `getActiveTools`
//      / `getAllTools` data after registration (names, exposure, schema and
//      implementation digests, source metadata);
//   3. receive a fresh per-runtime observation key/challenge on the private
//      attestation socket and send HMAC-SHA256 authenticated
//      `cadence.runtime.registry/1` observations with a strictly increasing
//      counter.
//
// There is no eval, no sign/key-getter, no shell, no inspector, no command or
// tool registration, and no global exposure of the observation key: it lives in
// this module closure only. If any fact is missing the extension sends nothing
// and the owner withholds readiness.
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import { createRequire } from 'node:module';
import { VERSION } from '@earendil-works/pi-coding-agent';

const SEAL_ADDON = '/opt/cadence/node/cadence_seal.node';
const ATTEST_SOCKET = '/opt/cadence/run/attest.sock';
const OBSERVER_PATH = '/opt/cadence/node/observer.mjs';
const POLICY_PATH = '/opt/cadence/material/policy.json';
const PI_VERSION = '1.4.0';
const MAX_TOOL_FILE_BYTES = 1024 * 1024;
const OBSERVE_INTERVAL_MS = 30000;

const DOMAIN_REGISTRY = 'cadence.runtime.registry.v1\0';
const DOMAIN_TOOL_SCHEMA = 'cadence.tool.schema.v1\0';
const DOMAIN_TOOL_IMPL = 'cadence.tool.implementation.v1\0';

const sha256 = value =>
  'sha256:' + crypto.createHash('sha256').update(value).digest('hex');

const canonical = value => {
  if (value === null) return 'null';
  if (value === true) return 'true';
  if (value === false) return 'false';
  if (typeof value === 'number') {
    if (!Number.isSafeInteger(value)) throw new Error('non-integer JSON value');
    return String(value);
  }
  if (typeof value === 'string') return JSON.stringify(value);
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
  if (typeof value === 'object') {
    const keys = Object.keys(value).sort();
    return '{' + keys.map(key => JSON.stringify(key) + ':' + canonical(value[key])).join(',') + '}';
  }
  throw new Error('unsupported JSON value');
};

const nulList = path => {
  const raw = fs.readFileSync(path);
  const out = [];
  let start = 0;
  for (let i = 0; i < raw.length; i += 1) {
    if (raw[i] === 0) {
      if (i > start) out.push(raw.subarray(start, i));
      start = i + 1;
    }
  }
  if (start < raw.length) out.push(raw.subarray(start));
  return out;
};

const listDigest = path => {
  const hash = crypto.createHash('sha256');
  for (const entry of nulList(path)) {
    hash.update(entry);
    hash.update(Buffer.from([0]));
  }
  return 'sha256:' + hash.digest('hex');
};

// The seal makes this process non-dumpable, after which the kernel exposes its
// own `/proc/self/environ` as root-owned 0400 and the runtime uid can no longer
// read it. The environment is fixed at exec, so capture its exact kernel digest
// before the seal is applied and reuse it for every observation. The owner
// measures the same immutable bytes through `/proc/<pid>/environ`.
const ENV_DIGEST = listDigest('/proc/self/environ');

const statusFact = (status, name) => {
  const line = status.split('\n').find(entry => entry.startsWith(name + ':'));
  return line ? line.slice(name.length + 1).trim() : '';
};

const sealFacts = () => {
  assertEnvironment();
  const require = createRequire(import.meta.url);
  const addon = require(SEAL_ADDON);
  if (typeof addon.seal !== 'function') throw new Error('seal addon missing');
  const sealed = addon.seal();
  if (!sealed || sealed.dumpable !== 0 || sealed.core !== 0 || sealed.nnp !== 1) {
    throw new Error('seal facts are not dumpable=0/core=0/nnp=1');
  }
  const status = fs.readFileSync('/proc/self/status', 'utf8');
  const seccomp = Number(statusFact(status, 'Seccomp'));
  const caps = statusFact(status, 'CapEff');
  const noNewPrivs = Number(statusFact(status, 'NoNewPrivs'));
  if (seccomp !== 2 || noNewPrivs !== 1) throw new Error('guest filter/nnp not active');
  if (caps !== '0000000000000000') throw new Error('guest retains capabilities');
  const environ = fs.statSync('/proc/self/environ');
  if (environ.uid !== 0 || (environ.mode & 0o077) !== 0) {
    throw new Error('guest is dumpable');
  }
  return { dumpable: 0, core: 0, nnp: 1, seccomp, caps };
};

const assertEnvironment = () => {
  const forbidden = /^(LD_|NODE_OPTIONS$|NODE_DEBUG$|NODE_INSPECT$|NODE_EXTRA_CA_CERTS$)/;
  for (const key of Object.keys(process.env)) {
    if (forbidden.test(key)) throw new Error('loader/operator material in environment');
  }
  for (const arg of process.execArgv) {
    if (arg.startsWith('--inspect') || arg === '--eval' || arg.startsWith('--eval=')) {
      throw new Error('inspector/eval is enabled');
    }
  }
  if (process.pid !== 1) throw new Error('observer is not the guest init');
  if (VERSION !== PI_VERSION) throw new Error('Pi version is not the pinned 1.4.0');
};

const sourceKind = info => {
  const path = String(info?.path ?? '');
  if (path.startsWith('builtin:mcp')) return 'mcp';
  if (path.startsWith('builtin:')) return 'builtin';
  if (info?.origin === 'package') return 'package';
  return 'extension';
};

const implementationDigest = tool => {
  const info = tool.sourceInfo ?? {};
  const path = String(info.path ?? '');
  const material = Buffer.concat([
    Buffer.from(DOMAIN_TOOL_IMPL, 'utf8'),
    Buffer.from(path, 'utf8'),
    Buffer.from([0]),
    Buffer.from(String(tool.name ?? ''), 'utf8'),
  ]);
  if (path.startsWith('builtin:') || path.startsWith('<')) {
    return sha256(material);
  }
  let bytes;
  try {
    const stat = fs.statSync(path);
    if (!stat.isFile() || stat.size > MAX_TOOL_FILE_BYTES) throw new Error('tool source bound');
    bytes = fs.readFileSync(path);
  } catch {
    throw new Error('tool implementation is unreadable');
  }
  return sha256(Buffer.concat([material, Buffer.from([0]), bytes]));
};

const toolRegistry = pi => {
  const active = pi.getActiveTools();
  if (!Array.isArray(active) || active.length === 0) throw new Error('no active tools');
  const activeSet = new Set(active);
  const tools = pi
    .getAllTools()
    .map(tool => {
      const schema = canonical(tool.parameters ?? {});
      return {
        name: String(tool.name ?? ''),
        exposure: String(tool.exposure ?? ''),
        schema_digest: sha256(Buffer.concat([
          Buffer.from(DOMAIN_TOOL_SCHEMA, 'utf8'),
          Buffer.from(schema, 'utf8'),
        ])),
        implementation_digest: implementationDigest(tool),
        source_kind: sourceKind(tool.sourceInfo),
        source_path: String(tool.sourceInfo?.path ?? ''),
      };
    })
    .filter(tool => activeSet.has(tool.name))
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const names = tools.map(tool => tool.name);
  const sortedActive = [...active].sort();
  if (JSON.stringify(names) !== JSON.stringify(sortedActive)) {
    throw new Error('active tools and registry disagree');
  }
  return { tools, active: sortedActive };
};

const buildPayload = (pi, generation) => {
  const { tools, active } = toolRegistry(pi);
  const policy = fs.readFileSync(POLICY_PATH);
  return {
    v: 1,
    runtime_generation: generation,
    node_pid: process.pid,
    seal: sealFacts(),
    argv_digest: listDigest('/proc/self/cmdline'),
    env_digest: ENV_DIGEST,
    pi_version: PI_VERSION,
    observer_digest: sha256(fs.readFileSync(OBSERVER_PATH)),
    tools,
    active,
    policy_digest: sha256(policy),
  };
};

const registryDigest = payload =>
  sha256(Buffer.concat([
    Buffer.from(DOMAIN_REGISTRY, 'utf8'),
    Buffer.from(canonical(payload), 'utf8'),
  ]));

const b64url = buffer =>
  buffer.toString('base64').replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');

export default function install(pi) {
  if (VERSION !== PI_VERSION) return;
  // Apply and verify the post-exec seal before this observer connects for the
  // private observation key. Every later observation re-proves it in
  // buildPayload; the root owner also checks the kernel proof before issuing.
  sealFacts();
  let key = null;
  let challenge = null;
  let generation = null;
  let counter = 0;
  let socket = null;
  let timer = null;

  const send = () => {
    if (!socket || !key || !challenge) return;
    const payload = buildPayload(pi, generation);
    counter += 1;
    const digest = registryDigest(payload);
    const material = Buffer.from(
      `cadence.runtime.registry/1\n${generation}\n${challenge}\n${counter}\n${digest}\n`,
      'utf8',
    );
    const mac = crypto.createHmac('sha256', key).update(material).digest();
    socket.write(JSON.stringify({
      v: 1,
      payload: { ...payload, counter },
      digest,
      mac: b64url(mac),
    }) + '\n');
  };

  const receiveIssue = frame => {
    if (!frame || frame.v !== 1 || frame.kind !== 'issue') throw new Error('bad issue');
    if (!Number.isSafeInteger(frame.runtime_generation) || frame.runtime_generation <= 0) {
      throw new Error('bad generation');
    }
    generation = frame.runtime_generation;
    challenge = String(frame.challenge ?? '');
    if (!/^[0-9a-f]{64}$/.test(challenge)) throw new Error('bad challenge');
    key = Buffer.from(String(frame.key ?? ''), 'base64url');
    if (key.length !== 32) throw new Error('bad observation key');
    send();
    timer = setInterval(send, OBSERVE_INTERVAL_MS);
  };

  let buffer = '';
  socket = net.connect(ATTEST_SOCKET);
  socket.setEncoding('utf8');
  socket.on('data', chunk => {
    buffer += chunk;
    for (;;) {
      const newline = buffer.indexOf('\n');
      if (newline < 0) break;
      const line = buffer.slice(0, newline);
      buffer = buffer.slice(newline + 1);
      try {
        receiveIssue(JSON.parse(line));
      } catch {
        socket.destroy();
        return;
      }
    }
  });
  socket.on('error', () => clearInterval(timer));
  socket.on('close', () => clearInterval(timer));
}
