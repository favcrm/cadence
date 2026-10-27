#!/usr/bin/env node
// CAD-1085 r7 — the reviewed protected guest Pi bootstrap.
//
// This is the ONLY entry the measured OCI runtime launches. It uses the public
// Pi 1.4.0 SDK surface (`SettingsManager`, `SessionManager`,
// `createAgentSessionServices`, `createAgentSessionFromServices`,
// `createAgentSessionRuntime`, `runRpcMode`) and nothing else:
//
//   * no shell, no CLI argument parsing beyond the fixed reviewed flags, no
//     eval, no inspector, no project/global extension discovery;
//   * an EMPTY CredentialStore — the guest holds no provider secret, ever;
//   * immutable effective settings applied before session creation and
//     verified after: retry off (agent + provider), compaction off,
//     cacheWarming off;
//   * exactly one reviewed resource graph: the observer extension passed with
//     `-e` plus the keyless native-model provider bridge from the pinned
//     material root;
//   * provider/model/effort come from the immutable operator-provisioned
//     `session.json`, never from the prompt, env or the model.
//
// A missing/mismatched fact refuses at startup (exit 1, fixed message). The
// guest never falls back to host networking, host auth files or a built-in
// provider.
import fs from 'node:fs';
import process from 'node:process';

import {
  createAgentSessionFromServices,
  createAgentSessionRuntime,
  createAgentSessionServices,
  ModelRuntime,
  runRpcMode,
  SettingsManager,
  SessionManager,
} from '@earendil-works/pi-coding-agent';
import * as piAi from '@earendil-works/pi-ai';
import { Compile } from 'typebox/compile';

import {
  createNativeConnectionTools,
  createNativeModelProvider,
} from '/opt/cadence/material/model-bridge.mjs';

const OPS_SOCKET = '/opt/cadence/run/ops.sock';
const SESSION_CONFIG = '/opt/cadence/material/guest/session.json';
const WORKSPACE = '/workspace';
const AGENT_DIR = '/home/agent/pi';
const PI_VERSION = '1.4.0';
const PROVIDER = 'devin';
const THINKING_LEVELS = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
const TOOL_NAME = /^[A-Za-z0-9_-]{1,64}$/;

const FIXED_ERRORS = {
  args: 'protected guest bootstrap refused its arguments',
  config: 'protected guest session configuration is unavailable',
  settings: 'protected guest settings could not be applied',
  model: 'protected guest model provider is unavailable',
  runtime: 'protected guest runtime could not be created',
};

const fail = (key) => {
  process.stderr.write(`cadence-guest: ${FIXED_ERRORS[key] ?? FIXED_ERRORS.runtime}\n`);
  process.exit(1);
};

const readConfig = () => {
  const raw = fs.readFileSync(SESSION_CONFIG, 'utf8');
  if (raw.length > 64 * 1024) throw new Error('config bound');
  const value = JSON.parse(raw);
  if (!value || value.v !== 1 || typeof value !== 'object') throw new Error('config shape');
  const keys = ['v', 'provider', 'model_uid', 'effort', 'max_output_tokens', 'max_context_tokens', 'tools'];
  for (const key of Object.keys(value)) {
    if (!keys.includes(key)) throw new Error('config extra field');
  }
  if (value.provider !== PROVIDER) throw new Error('provider');
  if (typeof value.model_uid !== 'string' || !/^[A-Za-z0-9._-]{1,128}$/.test(value.model_uid)) {
    throw new Error('model uid');
  }
  if (!Number.isSafeInteger(value.max_output_tokens) || value.max_output_tokens < 1 || value.max_output_tokens > 8192) {
    throw new Error('max output');
  }
  if (!Number.isSafeInteger(value.max_context_tokens) || value.max_context_tokens < 1 || value.max_context_tokens > 4_000_000) {
    throw new Error('max context');
  }
  if (value.effort !== null && (typeof value.effort !== 'string' || !THINKING_LEVELS.has(value.effort))) {
    throw new Error('effort');
  }
  if (!Array.isArray(value.tools) || value.tools.length === 0 || value.tools.length > 16) {
    throw new Error('tools');
  }
  for (const tool of value.tools) {
    if (typeof tool !== 'string' || !TOOL_NAME.test(tool)) throw new Error('tool name');
  }
  return value;
};

const parseArgs = (argv) => {
  const extensions = [];
  let noExtensions = false;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--no-extensions') {
      noExtensions = true;
      continue;
    }
    if (arg === '-e' || arg === '--extension') {
      const path = argv[index + 1];
      if (typeof path !== 'string' || !path.startsWith('/opt/cadence/') || path.includes('..')) {
        throw new Error('extension path');
      }
      extensions.push(path);
      index += 1;
      continue;
    }
    throw new Error('argument');
  }
  if (extensions.length === 0) throw new Error('no observer extension');
  return { extensions, noExtensions };
};

const assertEnvironment = () => {
  const forbidden = /^(LD_|NODE_OPTIONS$|NODE_DEBUG$|NODE_INSPECT$|NODE_PATH$|NODE_EXTRA_CA_CERTS$)/;
  for (const key of Object.keys(process.env)) {
    if (forbidden.test(key)) throw new Error('environment');
  }
  for (const arg of process.execArgv) {
    if (arg.startsWith('--inspect') || arg === '--eval' || arg.startsWith('--eval=')) {
      throw new Error('inspector');
    }
  }
  if (process.pid !== 1) throw new Error('guest init');
};

// The mandatory empty credential store: there is no guest credential. A
// mutation attempt is a refusal, not a write.
const emptyCredentials = Object.freeze({
  async read() {
    return undefined;
  },
  async list() {
    return [];
  },
  async modify() {
    throw new Error('guest credential store is empty');
  },
  async delete() {
    throw new Error('guest credential store is empty');
  },
});

const reviewSettings = (manager) => {
  const settings = manager.getSettings();
  const retry = settings.retry ?? {};
  const providerRetry = retry.provider ?? {};
  if (
    retry.enabled !== false ||
    retry.maxRetries !== 0 ||
    providerRetry.maxRetries !== 0 ||
    (settings.compaction ?? {}).enabled !== false ||
    settings.cacheWarming !== 'off'
  ) {
    throw new Error('effective settings');
  }
};

const buildModel = (config) => ({
  id: config.model_uid,
  name: config.model_uid,
  api: 'pi-messages',
  provider: PROVIDER,
  baseUrl: 'https://protected.invalid',
  input: ['text'],
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  reasoning: config.effort !== null,
  contextWindow: config.max_context_tokens,
  maxTokens: config.max_output_tokens,
});

async function main() {
  assertEnvironment();
  const args = parseArgs(process.argv.slice(2));
  const config = readConfig();
  const model = buildModel(config);
  fs.mkdirSync(AGENT_DIR, { recursive: true, mode: 0o700 });

  const createRuntime = async ({ cwd, sessionManager }) => {
    const settingsManager = SettingsManager.inMemory({
      retry: { enabled: false, maxRetries: 0, provider: { maxRetries: 0 } },
      compaction: { enabled: false },
      cacheWarming: 'off',
    });
    reviewSettings(settingsManager);
    const modelRuntime = await ModelRuntime.create({
      credentials: emptyCredentials,
      modelsPath: null,
      allowModelNetwork: false,
      refreshOnCreate: false,
    });
    modelRuntime.registerNativeProvider(
      createNativeModelProvider({
        socketPath: OPS_SOCKET,
        provider: PROVIDER,
        model,
        effort: config.effort,
        piAi,
      }),
    );
    const services = await createAgentSessionServices({
      cwd,
      agentDir: AGENT_DIR,
      settingsManager,
      modelRuntime,
      resourceLoaderOptions: {
        additionalExtensionPaths: args.extensions,
        noExtensions: args.noExtensions,
        noSkills: true,
        noPromptTemplates: true,
        noThemes: true,
        noContextFiles: true,
      },
    });
    reviewSettings(services.settingsManager);
    const connectionTools = createNativeConnectionTools({
      socketPath: OPS_SOCKET,
      compileSchema: Compile,
    });
    const created = await createAgentSessionFromServices({
      services,
      sessionManager,
      model,
      thinkingLevel: config.effort ?? undefined,
      tools: [...new Set([...config.tools, ...connectionTools.map(tool => tool.name)])],
      customTools: connectionTools,
    });
    return { ...created, services, diagnostics: [] };
  };

  const sessionManager = SessionManager.inMemory(WORKSPACE);
  const runtime = await createAgentSessionRuntime(createRuntime, {
    cwd: WORKSPACE,
    agentDir: AGENT_DIR,
    sessionManager,
  });
  for (const diagnostic of runtime.diagnostics) {
    if (diagnostic.type === 'error') throw new Error('runtime diagnostic');
  }
  await runRpcMode(runtime);
}

try {
  if (process.argv.length < 3) fail('args');
  await main();
} catch {
  fail('runtime');
}
