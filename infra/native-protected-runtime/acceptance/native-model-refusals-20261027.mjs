#!/usr/bin/env node
// CAD-1085 refusal acceptance for the runtime operations socket.
//
// Run inside the enrolled runtime's PID namespace as the enrolled uid (the
// fixture runs it with `docker exec -u <guest uid>`); the socket only admits
// that peer. Every case must be refused and the refused code is published.
//
// Usage: node native-model-refusals-20261027.mjs <ops-socket-path>
// Prints one JSON document: [{case, outcome, code?, message?}]
const net = require('node:net');

// `node -e <code> <socket>` has no script path, so the socket is argv[1]; a
// file invocation carries it at argv[2].
const SOCK = process.argv.slice(1).find(arg => !arg.endsWith('.mjs'));
if (!SOCK) {
  console.error('usage: native-model-refusals-20261027.mjs <ops-socket-path>');
  process.exit(2);
}

const call = (payload, name) =>
  new Promise(resolve => {
    const socket = net.connect(SOCK);
    let buf = '';
    let done = false;
    const finish = result => {
      if (done) return;
      done = true;
      socket.destroy();
      resolve({ case: name, ...result });
    };
    socket.setTimeout(5000, () => finish({ outcome: 'no_reply', code: 'timeout' }));
    socket.on('connect', () => socket.write(payload));
    socket.on('data', chunk => {
      buf += chunk.toString('utf8');
    });
    socket.on('end', () => finish(classify(name, buf)));
    socket.on('close', () => finish(classify(name, buf)));
    socket.on('error', error => finish({ outcome: 'refused', code: String(error.code ?? error.message) }));
  });

const classify = (name, buf) => {
  const line = buf.trim();
  if (!line) return { outcome: 'refused', code: 'closed_without_reply' };
  try {
    const reply = JSON.parse(line);
    if (reply.ok === false && reply.error) {
      return { outcome: 'refused', code: String(reply.error.code ?? 'error'), message: String(reply.error.message ?? '') };
    }
    return { outcome: 'accepted', code: null, reply: line.slice(0, 200) };
  } catch {
    return { outcome: 'refused', code: 'non_json_reply', reply: line.slice(0, 120) };
  }
};

const frame = value => JSON.stringify(value) + '\n';

(async () => {
  const results = [];
  results.push(await call(frame({ v: 1, method: 'bogus_v1', args: {} }), 'unknown_method'));
  results.push(
    await call(
      frame({ v: 1, method: 'native_model_turn_prepare_v1', args: { v: 1, request_index: 0 } }),
      'prepare_from_unmeasured_process',
    ),
  );
  results.push(await call('x'.repeat(1024 * 1024 + 64) + '\n', 'oversized_frame'));
  results.push(await call('{}\n{}\n', 'trailing_bytes'));
  results.push(await call('\n', 'empty_frame'));
  process.stdout.write(JSON.stringify(results) + '\n');
})();
