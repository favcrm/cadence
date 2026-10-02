#!/usr/bin/env python3
"""Opt-in real Pi guard probe, including asynchronous turn-end race."""
import json, os, pathlib, tempfile, time, uuid
from native_probe import Child, HOLD, PROMPT, AMEND, FOLLOW, busy, texts


def main():
    if os.environ.get('CAD1015_DOGFOOD') != '1':
        raise SystemExit('Opt-in required: CAD1015_DOGFOOD=1')
    root = pathlib.Path(tempfile.mkdtemp(prefix='c1015-guard-'))
    (root / 'hold.py').write_text(HOLD)
    # Loaded after the guard: creates a real awaited listener window while
    # the runtime still reports streaming but the turn has closed admission.
    (root / 'closing.ts').write_text('''import fs from "node:fs";
export default function(pi) {
  pi.on("context", event => {
    const turns = event.messages.filter(message => message.role === "custom"
      && message.customType === "cadence-turn-guidance").map(message => message.details?.turn);
    fs.appendFileSync("guidance-context.jsonl", JSON.stringify(turns) + "\\n");
  });
  let once = false;
  pi.on("turn_end", async () => {
    if (once) return;
    once = true;
    fs.writeFileSync("closing", "ready");
    const end = Date.now() + 25000;
    while (!fs.existsSync("closing-release") && Date.now() < end)
      await new Promise(resolve => setTimeout(resolve, 25));
  });
}
''')
    guard = pathlib.Path(__file__).resolve().parents[3] / 'src/adapter/pi_turn_guard.ts'
    argv = ['pi', '--mode', 'rpc', '--no-session', '--no-extensions',
            '-e', str(guard), '-e', str(root / 'closing.ts'),
            '-e', '/home/ubuntu/.pi/agent/npm/node_modules/pi-devin/extensions/index.ts',
            '--model', 'devin/swe-2-high', '--thinking', 'low', '--no-skills',
            '--no-prompt-templates', '--no-context-files', '--tools', 'bash']
    env = {k: v for k, v in os.environ.items() if not k.startswith('CADENCE_')}
    child = Child(argv, root, env)
    report = {'root': str(root), 'scope': 'real Pi runtime guard, not Cadence end-to-end', 'status': 'blocked'}

    def command(operation, turn, text=None, request=None):
        request = request or str(uuid.uuid4())
        value = {'request': request, 'turn': turn}
        if text is not None:
            value['text'] = text
        wire_id = str(uuid.uuid4())
        child.send({'id': wire_id, 'type': 'prompt', 'message': '/cadence-' + operation + '-turn ' + json.dumps(value)})
        reply = child.wait(lambda event: event.get('id') == wire_id, 10)
        assert reply.get('success'), reply
        receipts = [event['entry']['data'] for event in child.seen
                    if event.get('type') == 'entry_appended'
                    and event.get('entry', {}).get('customType') == 'cadence-turn-input'
                    and event['entry'].get('data', {}).get('request') == request]
        assert receipts, 'missing runtime receipt; prompt handled is not acceptance evidence'
        return receipts[-1]

    def clear_queue():
        wire_id = str(uuid.uuid4())
        child.send({'id': wire_id, 'type': 'clear_queue'})
        assert child.wait(lambda event: event.get('id') == wire_id, 10).get('success')

    def marker(name):
        deadline = time.monotonic() + 10
        while not (root / name).exists():
            if time.monotonic() > deadline:
                raise TimeoutError('missing owned marker ' + name)
            time.sleep(.025)

    def prompt(message):
        wire_id = str(uuid.uuid4())
        child.send({'id': wire_id, 'type': 'prompt', 'message': message})
        assert child.wait(lambda event: event.get('id') == wire_id).get('success')

    try:
        assert command('steer', 'idle', 'IDLE_CAD1015')['outcome'] == 'skipped_inactive'
        assert command('bind', 'unused-binding')['outcome'] == 'bound'
        assert command('abandon', 'foreign-binding')['outcome'] == 'skipped_inactive'
        assert command('abandon', 'unused-binding')['outcome'] == 'cleared'
        assert command('bind', 'pi-owned-turn1')['outcome'] == 'bound'
        prompt(PROMPT)
        busy(child, 'pi')
        marker('busy')
        assert command('abandon', 'pi-owned-turn1')['outcome'] == 'rejected'
        assert command('steer', 'pi-other-generation', 'FORGED_CAD1015')['outcome'] == 'skipped_inactive'
        request = str(uuid.uuid4())
        assert command('steer', 'pi-owned-turn1', AMEND, request)['outcome'] == 'queued'
        duplicate = command('steer', 'pi-owned-turn1', AMEND, request)
        assert duplicate['outcome'] == 'queued' and duplicate['reason'] == 'duplicate'
        (root / 'release').write_text('release')
        marker('closing')
        assert command('steer', 'pi-owned-turn1', 'LATE_CAD1015')['outcome'] == 'skipped_inactive'
        (root / 'closing-release').write_text('release')
        child.wait(lambda event: event.get('type') == 'agent_settled')
        clear_queue()
        report['initial_outputs'] = texts(child.seen, 'pi')
        assert 'BASELINE AMENDED_CAD1015' in report['initial_outputs']
        assert command('steer', 'pi-owned-turn1', 'STALE_CAD1015')['outcome'] == 'skipped_inactive'
        assert command('bind', 'pi-owned-turn2')['outcome'] == 'bound'
        assert command('steer', 'pi-owned-turn1', 'STALE_CAD1015')['outcome'] == 'skipped_inactive'
        (root / 'busy').unlink()
        (root / 'release').unlink()
        prompt(PROMPT)
        busy(child, 'pi')
        marker('busy')
        assert command('steer', 'pi-owned-turn2', 'ABORT_LEAK_CAD1015')['outcome'] == 'queued'
        child.send({'type': 'abort'})
        child.wait(lambda event: event.get('type') == 'agent_settled')
        clear_queue()
        assert command('bind', 'pi-owned-turn3')['outcome'] == 'bound'
        projection_start = len((root / 'guidance-context.jsonl').read_text().splitlines())
        start = len(child.seen)
        prompt(FOLLOW)
        child.wait(lambda event: event.get('type') == 'agent_settled')
        report['followup_outputs'] = texts(child.seen[start:], 'pi')
        assert report['followup_outputs'] == ['FOLLOWUP_CAD1015']
        projected = [json.loads(line) for line in (root / 'guidance-context.jsonl').read_text().splitlines()]
        assert projected and any('pi-owned-turn1' in turns for turns in projected), 'missing active-run projection proof'
        report['guidance_context_turns'] = projected
        report['successor_guidance_context_turns'] = projected[projection_start:]
        assert projected[projection_start:] == [[]], 'old guidance leaked or induced an extra successor model request'
        report.update(status='observed', cancelled_run_guidance_filtered=True, idle_refused=True, stale_refused=True,
                      foreign_generation_refused=True, closing_listener_race_refused=True,
                      duplicate_deduped=True, no_leak_into_next_run=True)
    except Exception as error:
        report['error'] = str(error)
    finally:
        (root / 'release').write_text('cleanup')
        (root / 'closing-release').write_text('cleanup')
        child.stop()
        report['process_exit'] = child.p.returncode
        (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report, indent=2))
    return 0 if report['status'] == 'observed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
