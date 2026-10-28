import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';

// Independent negative acceptance. No daemon-private guard is imported or copied.
const FIX = '/srv/cad1085-e2e';
let phase = 'fixture safety';
function requireThat(ok, message) {
  if (!ok) throw new Error(message);
}
function fixturePath(p) {
  requireThat(typeof p === 'string' && p.startsWith(`${FIX}/`), 'outside fixture');
  requireThat(path.resolve(p) === p && fs.realpathSync(p) === p, 'noncanonical fixture path');
  return p;
}
function checkedNode(p, kind, uid, gid, mode) {
  fixturePath(p);
  const st = fs.lstatSync(p);
  requireThat(!st.isSymbolicLink(), 'symlink');
  requireThat(kind === 'file' ? st.isFile() : kind === 'dir' ? st.isDirectory() : st.isSocket(), 'wrong inode type');
  requireThat(st.uid === uid && (gid === null || st.gid === gid), 'wrong inode owner');
  if (mode !== null) requireThat((st.mode & 0o777) === mode, 'wrong inode mode');
  return st;
}

const PY = String.raw`
import hashlib, json, os, socket, sqlite3, struct, sys
from urllib.parse import quote

def starttime(pid):
    with open('/proc/%d/stat' % pid) as f:
        raw = f.read()
    return int(raw[raw.rfind(')') + 2:].split()[19])

def peer(s, measure):
    pid, uid, gid = struct.unpack('3i', s.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize('3i')))
    result = dict(pid=pid, uid=uid, gid=gid, starttime=starttime(pid))
    if measure:
        h = hashlib.sha256()
        with open('/proc/%d/exe' % pid, 'rb') as f:
            for chunk in iter(lambda: f.read(65536), b''):
                h.update(chunk)
        result['digest'] = 'sha256:' + h.hexdigest()
        if starttime(pid) != result['starttime']:
            raise RuntimeError('changed peer')
    return result

def run(a):
    mode = a['mode']
    if mode == 'snapshot':
        db = sqlite3.connect('file:' + quote(a['db'], safe='/') + '?mode=ro', uri=True, timeout=2)
        db.execute('PRAGMA query_only=ON')
        db.execute('BEGIN')
        selections = {
          'owner_meta': 'id,workspace_id,owner_epoch,database_epoch,restore_state,boot_id',
          'runtime_obligations': 'runtime_id,generation,owner_epoch,phase,observation_counter,loaded_receipt_id,node_pid,node_starttime,qualified_at_ms,updated_at_ms',
          'obligation_transitions': 'id,runtime_id,from_phase,to_phase,at_ms',
          'uid_allocations': 'os_uid,runtime_id',
          'owner_watermark': 'id,minimum_sequence,minimum_digest,database_epoch',
          'owner_checkpoints': 'sequence,digest,database_epoch,runtime_binding_digest,at_ms',
          'restore_log': 'id,action,owner_epoch,database_epoch,at_ms',
          'peer_enrollments': 'role,generation,pid,starttime,exe_digest'
        }
        rows = {}
        for table, columns in selections.items():
            data = db.execute('SELECT ' + columns + ' FROM ' + table + ' ORDER BY rowid LIMIT 4097').fetchall()
            if len(data) > 4096:
                raise RuntimeError('snapshot bound')
            rows[table] = data
        meta = db.execute('SELECT workspace_id,owner_epoch,restore_state FROM owner_meta WHERE id=1').fetchone()
        if meta is None:
            raise RuntimeError('missing owner')
        busy = db.execute("SELECT count(*) FROM runtime_obligations WHERE phase IN ('allocating','prepared','consumed','starting','observing')").fetchone()[0]
        supervisors = db.execute("SELECT pid,starttime FROM peer_enrollments WHERE role='supervisor'").fetchall()
        digest = hashlib.sha256(json.dumps(rows, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        db.close()
        return dict(hash=digest, workspace=meta[0], epoch=meta[1], restore=meta[2], busy=busy,
                    runtime_ids=[r[0] for r in rows['runtime_obligations']], supervisors=supervisors)
    if mode == 'owner':
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(5)
            s.connect(a['socket'])
            return peer(s, True)
    if mode != 'exchange':
        raise RuntimeError('unknown mode')
    if a['nonroot']:
        os.setgroups([])
        os.setgid(a['gid'])
        os.setuid(a['uid'])
    caller = dict(pid=os.getpid(), uid=os.geteuid(), starttime=starttime(os.getpid()))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(5)
        # A connect failure is an error, never evidence of application refusal.
        s.connect(a['socket'])
        server = peer(s, not a['nonroot'])
        if server['uid'] != a['uid'] or server['gid'] != a['gid']:
            raise RuntimeError('wrong callback server')
        if not a['nonroot'] and server['digest'] != a['digest']:
            raise RuntimeError('wrong callback executable')
        if a.get('expected') and any(server[k] != a['expected'][k] for k in ('pid','uid','gid','starttime')):
            raise RuntimeError('changed callback server')
        payload = json.dumps(a['request'], separators=(',', ':')).encode() + b'\n'
        try:
            s.sendall(payload)
        except (BrokenPipeError, ConnectionResetError):
            if not a['nonroot']:
                raise
        data = b''
        end = 'eof'
        while True:
            try:
                chunk = s.recv(4096)
            except ConnectionResetError:
                end = 'reset'
                break
            if not chunk:
                break
            data += chunk
            if len(data) > 8192:
                raise RuntimeError('response bound')
        return dict(caller=caller, server=server, text=data.decode('utf-8'), end=end)

try:
    result = run(json.load(sys.stdin))
    print(json.dumps(result, separators=(',', ':')))
except Exception:
    # Do not disclose request/key bytes or exception representations.
    print('{"error":"probe_failed"}')
    sys.exit(1)
`;
function probe(mode, args) {
  return JSON.parse(execFileSync('/usr/bin/python3', ['-c', PY], {
    input: JSON.stringify({ mode, ...args }), encoding: 'utf8', timeout: 15000,
    maxBuffer: 1024 * 1024, stdio: ['pipe', 'pipe', 'pipe'],
  }));
}
function samePeer(a, b) {
  return ['pid', 'uid', 'gid', 'starttime', 'digest'].every(k => a[k] === b[k]);
}

try {
  requireThat(process.platform === 'linux' && process.geteuid() === 0, 'Linux root required');
  requireThat(process.argv.length === 4 && process.argv[2] === '--fixture' && process.argv[3] === FIX, 'exact fixture required');
  requireThat(fs.realpathSync(FIX) === FIX, 'fixture root redirected');
  checkedNode(`${FIX}/.cad1085-native-model-e2e`, 'file', 0, null, null);
  const config = JSON.parse(fs.readFileSync(fixturePath(`${FIX}/run-config.json`), 'utf8'));
  requireThat(config.board_port === 3141 && config.entry_port === 3142 && config.devin_port === 3121, 'wrong fixture ports');
  checkedNode(`${FIX}/profile/profile.json`, 'file', 0, null, 0o644);
  const profile = JSON.parse(fs.readFileSync(`${FIX}/profile/profile.json`, 'utf8'));
  const uid = profile.supervisor.uid;
  const gid = profile.supervisor.gid;
  requireThat(profile.deployment_class === 'native-oci-agent/1' && uid === 21000 && Number.isSafeInteger(gid) && gid > 0, 'wrong profile');
  requireThat(profile.operator_origin === 'https://entry.localhost:3142', 'wrong entry origin');
  const run = `${FIX}/profile/run`;
  const state = `${FIX}/profile/state`;
  const control = `${run}/control.sock`;
  const callback = `${run}/broker/broker-fence.sock`;
  const database = `${state}/runtime-owner.sqlite`;
  const keyPath = `${FIX}/profile/owner.key`;
  requireThat(profile.paths.run_dir === run && profile.paths.state_dir === state && profile.paths.control_socket === control, 'profile path mismatch');
  requireThat(profile.owner.private_key_file === keyPath, 'nonfixture key');
  checkedNode(run, 'dir', 0, gid, 0o750);
  checkedNode(`${run}/broker`, 'dir', 0, gid, 0o770);
  checkedNode(control, 'socket', 0, gid, 0o660);
  checkedNode(callback, 'socket', uid, null, 0o600);
  checkedNode(state, 'dir', 0, null, 0o700);
  checkedNode(database, 'file', 0, null, 0o600);
  const keyStat = checkedNode(keyPath, 'file', 0, null, 0o600);
  requireThat(keyStat.size > 0 && keyStat.size <= 4096, 'key size');

  phase = 'live owner and quiet authority';
  const before = probe('snapshot', { db: database });
  requireThat(before.workspace === profile.workspace_id && before.restore === 'stable' && before.busy === 0, 'fixture not quiescent');
  requireThat(Number.isSafeInteger(before.epoch) && before.epoch > 0, 'invalid epoch');
  const owner = probe('owner', { socket: control });
  requireThat(owner.uid === 0 && owner.pid > 0 && owner.pid !== process.pid && owner.digest === profile.owner.exe_digest, 'wrong live owner');

  phase = 'fixture-only signing';
  const bytes = fs.readFileSync(keyPath);
  let privateKey;
  try { privateKey = crypto.createPrivateKey({ key: bytes, format: 'der', type: 'pkcs8' }); }
  finally { bytes.fill(0); }
  const rawPublic = Buffer.from(profile.owner.public_key, 'base64url');
  requireThat(rawPublic.length === 32, 'public pin size');
  const pinnedDer = Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), rawPublic]);
  const publicKey = crypto.createPublicKey({ key: pinnedDer, format: 'der', type: 'spki' });
  requireThat(crypto.createPublicKey(privateKey).export({ format: 'der', type: 'spki' }).equals(pinnedDer), 'key/pin mismatch');
  function signedRequest() {
    const doc = { v: 1, kind: 'broker_fence', runtime_id: crypto.randomUUID(), owner_epoch: before.epoch,
      expected_generation: 1, reason: 'independent wrong-peer callback acceptance', nonce: crypto.randomUUID() };
    requireThat(!before.runtime_ids.includes(doc.runtime_id), 'existing runtime selector');
    // ASCII fields and safe integers use the protocol's sorted-key canonical JSON.
    const canonical = JSON.stringify(Object.fromEntries(Object.keys(doc).sort().map(k => [k, doc[k]])));
    const material = Buffer.concat([Buffer.from('cadence.broker-fence.v1\0'), Buffer.from(canonical)]);
    const signature = crypto.sign(null, material, privateKey);
    requireThat(crypto.verify(null, material, publicKey, signature), 'local signature invalid');
    return { ...doc, signature: signature.toString('base64url') };
  }
  const common = { socket: callback, uid, gid, digest: profile.supervisor.exe_digest };

  phase = 'valid signed wrong-root-PID callback refusal';
  const root = probe('exchange', { ...common, nonroot: false, request: signedRequest() });
  requireThat(root.caller.uid === 0 && root.caller.pid !== owner.pid, 'caller is not a different root PID');
  requireThat(before.supervisors.some(([pid, start]) => pid === root.server.pid && start === root.server.starttime), 'callback server not enrolled');
  requireThat(root.text.endsWith('\n'), 'missing structured root refusal');
  const reply = JSON.parse(root.text);
  requireThat(reply.ok === false && !Object.hasOwn(reply, 'data'), 'successful fence ACK');
  requireThat(reply.error?.v === 1 && reply.error.code === 'management_forbidden' && reply.error.message === 'broker fence refused', 'wrong refusal');

  phase = 'actual nonroot pre-parse callback refusal';
  const nonroot = probe('exchange', { ...common, nonroot: true, expected: root.server, request: signedRequest() });
  requireThat(nonroot.caller.uid === uid, 'UID drop failed');
  requireThat(nonroot.text === '' && ['eof', 'reset'].includes(nonroot.end), 'nonroot was not refused before parsing');

  phase = 'no durable authority promotion and stable peers';
  const after = probe('snapshot', { db: database });
  requireThat(after.hash === before.hash && after.epoch === before.epoch, 'owner authority changed');
  const ownerAfter = probe('owner', { socket: control });
  requireThat(samePeer(owner, ownerAfter), 'owner changed during acceptance');
  console.log(JSON.stringify({ check: 'root-callback-repair-negative-20261027', ok: true,
    wrong_root_pid_refused: true, actual_nonroot_refused_before_parse: true,
    successful_fence_ack: false, durable_owner_authority_unchanged: true,
    fixture: FIX, owner_epoch: before.epoch }));
} catch {
  console.error(`FAIL root-callback-repair-negative-20261027 at ${phase}; no acceptance PASS`);
  process.exitCode = 1;
}