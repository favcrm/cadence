#!/usr/bin/env python3
"""Baseline end-to-end inbox validation on a NEW temporary Cadence daemon."""
import json,os,pathlib,shutil,signal,socket,subprocess,tempfile,time,uuid
if os.environ.get('CAD1015_DOGFOOD') != '1':
 raise SystemExit('Opt-in required: CAD1015_DOGFOOD=1 (starts owned test daemon and real model sessions)')
PROVIDERS=os.environ.get('CAD1015_PROVIDERS','pi,codex,claude').split(',')
if any(p not in ('pi','codex','claude') for p in PROVIDERS):
 raise SystemExit('CAD1015_PROVIDERS must contain only pi,codex,claude')
ROOT=pathlib.Path(tempfile.mkdtemp(prefix='c1015-e2e-'))
STATE=ROOT/'state';PM=ROOT/'pm';PM.mkdir();STATE.mkdir()
(PM/'pm.yaml').write_text('pi:\n  providers:\n    - pi-devin@0.2.1\n  models:\n    allow: [devin/swe-2-high]\n    worker_allow: [devin/swe-2-high]\nhost:\n  confine_pi_workers: false\n')
env=os.environ.copy()
for k in list(env):
 if k.startswith('CADENCE_') or k=='CODEX_UNSAFE_ALLOW_NO_SANDBOX':env.pop(k,None)
env.update(CADENCE_PM_DIR=str(PM),CADENCE_STATE_DIR=str(STATE),CADENCE_SUITE_LOCK=str(ROOT/'suite.lock'))
CREDENTIAL_COPIES=[]
for variable,directory,filename,source in (
 ('CODEX_HOME','codex-home','auth.json',pathlib.Path.home()/'.codex/auth.json'),
 ('CLAUDE_CONFIG_DIR','claude-home','.credentials.json',pathlib.Path.home()/'.claude/.credentials.json'),
):
 home=ROOT/directory;home.mkdir();env[variable]=str(home)
 if source.exists():
  target=home/filename;shutil.copyfile(source,target);target.chmod(0o600);CREDENTIAL_COPIES.append(target)
(ROOT/'codex-home/config.toml').write_text('model = "gpt-6.1-sol"\nmodel_reasoning_effort = "low"\n')
# Keep runtime state isolated; no production socket is ever selected.
BINARY=os.environ.get('CAD1015_BINARY','cadence')
VERSION=subprocess.run([BINARY,'--version'],capture_output=True,text=True,check=True,timeout=10).stdout.strip()
CAD=[BINARY,'--state-dir',str(STATE)]
log=(ROOT/'daemon.log').open('wb')
daemon=subprocess.Popen(CAD+['daemon','run'],cwd=ROOT,env=env,stdout=log,stderr=log,start_new_session=True)
reports=[]
def cli(args,allow_error=False,timeout=100):
 p=subprocess.run(CAD+args,cwd=ROOT,env=env,capture_output=True,text=True,timeout=timeout)
 if p.returncode and not allow_error:raise RuntimeError(f'{args[:3]} exit {p.returncode}: {p.stderr[:800]} {p.stdout[:400]}')
 try:out=json.loads(p.stdout or p.stderr)
 except ValueError:
  try:out=[json.loads(line) for line in p.stdout.splitlines() if line]
  except ValueError:out=p.stdout or p.stderr
 return p.returncode,out

def rpc(method,params):
 with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as s:
  s.settimeout(45);s.connect(str(STATE/'cadence.sock'))
  s.sendall((json.dumps({'jsonrpc':'2.0','id':1,'method':method,'params':params})+'\n').encode())
  response=json.loads(s.makefile('rb').readline())
  if 'error' in response:raise RuntimeError(str(response['error']))
  return response.get('result',response)
try:
 end=time.monotonic()+15
 while not (STATE/'cadence.sock').exists():
  if daemon.poll() is not None:raise RuntimeError('temporary daemon exited; inspect owned daemon.log')
  if time.monotonic()>end:raise RuntimeError('temporary daemon readiness timeout')
  time.sleep(.1)
 cli(['agent','register','observer','--provider','inbox'])
 for provider in PROVIDERS:
  report={'provider':provider,'scope':'Cadence baseline inbox, installed binary; not new feature','status':'blocked'}
  reports.append(report)
  work=ROOT/provider;work.mkdir()
  (work/'hold.py').write_text("import pathlib,time\np=pathlib.Path('.')\np.joinpath('busy').write_text('ready')\nend=time.monotonic()+60\nwhile not p.joinpath('release').exists() and time.monotonic()<end: time.sleep(.05)\nprint('HOLD_RELEASED')\n")
  try:
   args=['agent','register',provider+'-probe','--provider',provider,'--endpoint','managed','--cwd',str(work),'--sandbox','workspace-write']
   if provider=='pi':args+=['--param','model=devin/swe-2-high','--param','effort=low']
   if provider=='codex':args+=['--param','model=gpt-6.1-sol','--param','effort=low','--param','approval_policy=never']
   if provider=='claude':args+=['--param','permission_mode=dontAsk','--param','allowed_tools=["Bash(python3 hold.py)"]']
   if provider=='claude':
    registration=rpc('agent_register',{'alias':'claude-probe','provider':'claude','endpoint_kind':'managed','cwd':str(work),'sandbox':'workspace-write','role':'worker','params':json.dumps({'permission_mode':'dontAsk','allowed_tools':['Bash(python3 hold.py)']})})
   else:_,registration=cli(args)
   report['registered']=True
   prompt='Isolated Cadence inbox probe. First run exactly `python3 hold.py` once and wait for its output; then reply exactly BASELINE. Do not read or edit other files, access the network, use git, or run additional commands. This is not development work.'
   msg='cad1015-'+provider+'-initial-'+str(uuid.uuid4())
   _,sent=cli(['send',provider+'-probe','--text',prompt,'--message',msg,'--reply-to','observer']);report['initial_send']=sent
   end=time.monotonic()+45
   while not (work/'busy').exists():
    if time.monotonic()>end:raise RuntimeError('owned busy marker not observed; inspect wait disposition')
    time.sleep(.1)
   report['busy_tool_observed']=True
   rc,nudge=cli(['send',provider+'-probe','--nudge','--text','Clarification: preserve the original objective and reply BASELINE AMENDED_CAD1015.'],allow_error=True)
   report['native_nudge_currently_refused']=rc!=0;report['nudge_refusal']=nudge
   amend='cad1015-'+provider+'-amend-'+str(uuid.uuid4())
   _,queued=cli(['send',provider+'-probe','--text','For this follow-up reply exactly BASELINE AMENDED_CAD1015. Do not use tools.','--message',amend,'--reply-to','observer']);report['amendment_send']=queued
   (work/'release').write_text('release')
   _,first=cli(['agent','wait',provider+'-probe','--message',msg,'--until','reported','--timeout','90s']);report['initial_wait']=first
   _,second=cli(['agent','wait',provider+'-probe','--message',amend,'--until','reported','--timeout','90s']);report['amendment_wait']=second
   _,peek=cli(['inbox','observer','--peek','--reader',provider+'-probe']);report['inbox_peek']=peek
   rows=peek if isinstance(peek,list) else [peek]
   assert len(rows)==2, f'expected two correlated results, got {len(rows)}'
   results=[json.loads(row['body'][row['body'].index('{'):]) for row in rows]
   by_message={r['message']:r['result'] for r in results}
   assert by_message[msg]['status']=='completed' and by_message[msg]['text'].strip()=='BASELINE', 'initial objective not completed as baseline'
   assert by_message[amend]['status']=='completed' and by_message[amend]['text'].strip()=='BASELINE AMENDED_CAD1015', 'follow-up result mismatch'
   assert by_message[msg]['turn_id']!=by_message[amend]['turn_id'], 'normal follow-up did not get a separate turn'
   _,again=cli(['inbox','observer','--peek','--reader',provider+'-probe'])
   assert again==peek, 'peek consumed or changed inbox results'
   _,ack=cli(['inbox','ack','observer',str(max(row['seq'] for row in rows)),'--reader',provider+'-probe']);report['inbox_ack']=ack
   _,empty=cli(['inbox','observer','--peek','--reader',provider+'-probe']);report['inbox_after_ack']=empty
   assert empty==[], 'ack watermark did not empty reader inbox'
   report['correlated_results_verified']=True
   report['peek_non_consuming_and_ack_verified']=True
   report['status']='observed'
  except Exception as e:report['error']=str(e)
  finally:
   (work/'release').write_text('cleanup-release')
   cli(['agent','stop',provider+'-probe'],allow_error=True,timeout=35)
finally:
 try:cli(['daemon','stop'],allow_error=True,timeout=35)
 except Exception:pass
 if daemon.poll() is None:
  os.killpg(daemon.pid,signal.SIGTERM)
  try:daemon.wait(timeout=5)
  except subprocess.TimeoutExpired:os.killpg(daemon.pid,signal.SIGKILL);daemon.wait(timeout=5)
 log.close()
 for credential in CREDENTIAL_COPIES:credential.unlink(missing_ok=True)
 report={'root':str(ROOT),'binary':BINARY,'version':VERSION,'daemon_exit':daemon.returncode,'results':reports}
 (ROOT/'report.json').write_text(json.dumps(report,indent=2)+'\n')
 print(json.dumps(report,indent=2))
if not reports or any(p['status']!='observed' for p in reports):raise SystemExit(1)
