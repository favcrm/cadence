#!/usr/bin/env python3
"""Owned, isolated native-protocol probes; NOT a Cadence end-to-end test."""
import argparse,json,os,pathlib,queue,shutil,signal,subprocess,threading,time,tempfile,uuid

HOLD='''import pathlib,time
p=pathlib.Path('.')
p.joinpath('busy').write_text('ready')
end=time.monotonic()+40
while not p.joinpath('release').exists() and time.monotonic()<end:
    time.sleep(.05)
if not p.joinpath('release').exists(): raise SystemExit('probe release timed out')
print('HOLD_RELEASED')
'''
PROMPT='This is an isolated messaging protocol test. Your only task: first run the exact shell command `python3 hold.py` once, wait for its output, then reply with exactly BASELINE. Do not read other files, edit files, access the network or run other commands. A later clarification may change the final reply; keep the original task of running the hold command.'
AMEND='Clarification for the same task: keep the hold-command objective, but change your final reply from BASELINE to BASELINE AMENDED_CAD1015. Do not run additional tools.'
FOLLOW='The previous task is finished. Reply with exactly FOLLOWUP_CAD1015. Do not run tools.'

class Child:
 def __init__(self,argv,cwd,env):
  self.p=subprocess.Popen(argv,cwd=cwd,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
  self.events=queue.Queue();self.errors=[];self.seen=[]
  def readout():
   for raw in self.p.stdout:
    try:self.events.put(json.loads(raw))
    except (ValueError,UnicodeDecodeError):self.events.put({'non_json':raw.decode(errors='replace')[:200]})
   self.events.put({'eof':True})
  def readerr():
   for raw in self.p.stderr:self.errors.append(raw.decode(errors='replace'))
  threading.Thread(target=readout,daemon=True).start();threading.Thread(target=readerr,daemon=True).start()
 def send(self,v):self.p.stdin.write((json.dumps(v)+'\n').encode());self.p.stdin.flush()
 def next(self,deadline):
  left=deadline-time.monotonic()
  if left<=0:raise TimeoutError('native event deadline')
  e=self.events.get(timeout=left);self.seen.append(e)
  if e.get('eof'):raise RuntimeError('provider process exited before expected event')
  return e
 def wait(self,fn,seconds=65):
  end=time.monotonic()+seconds
  while True:
   e=self.next(end)
   if fn(e):return e
 def request(self,i,method,params):
  self.send({'jsonrpc':'2.0','id':i,'method':method,'params':params})
  r=self.wait(lambda e:e.get('id')==i)
  if 'error' in r:raise RuntimeError(json.dumps(r['error']))
  return r.get('result',{})
 def stop(self):
  if self.p.poll() is None:
   try:self.p.stdin.close();self.p.wait(timeout=3)
   except subprocess.TimeoutExpired:
    os.killpg(self.p.pid,signal.SIGTERM)
    try:self.p.wait(timeout=3)
    except subprocess.TimeoutExpired:os.killpg(self.p.pid,signal.SIGKILL);self.p.wait(timeout=3)

def texts(events,provider):
 out=[]
 for e in events:
  if provider=='pi' and e.get('type')=='message_end' and e.get('message',{}).get('role')=='assistant':
   out.extend(b.get('text','') for b in e['message'].get('content',[]) if b.get('type')=='text')
  if provider=='codex' and e.get('method')=='item/completed':
   item=e.get('params',{}).get('item',{})
   if item.get('type')=='agentMessage':out.append(item.get('text',''))
  if provider=='claude' and e.get('type')=='assistant':
   out.extend(b.get('text','') for b in e.get('message',{}).get('content',[]) if b.get('type')=='text')
 return out

def busy(c,provider):
 def fn(e):
  if provider=='pi':return e.get('type')=='tool_execution_start'
  if provider=='codex':return e.get('method')=='item/started' and e.get('params',{}).get('item',{}).get('type')=='commandExecution'
  return e.get('type')=='assistant' and any(b.get('type')=='tool_use' for b in e.get('message',{}).get('content',[]))
 return c.wait(fn)

def probe(provider,root):
 d=root/provider;d.mkdir();(d/'hold.py').write_text(HOLD)
 env=os.environ.copy()
 for k in list(env):
  if k.startswith('CADENCE_') or k=='CODEX_UNSAFE_ALLOW_NO_SANDBOX':env.pop(k,None)
 env.update({'TMPDIR':str(d)})
 if provider=='pi':
  argv=['pi','--mode','rpc','--no-session','--no-extensions','-e','/home/ubuntu/.pi/agent/npm/node_modules/pi-devin/extensions/index.ts','--model','devin/swe-2-high','--thinking','low','--no-skills','--no-prompt-templates','--no-context-files','--tools','bash']
 elif provider=='codex':
  home=d/'home';home.mkdir();auth=pathlib.Path.home()/'.codex/auth.json'
  if auth.exists():
   shutil.copyfile(auth,home/'auth.json');(home/'auth.json').chmod(0o600)
  (home/'config.toml').write_text('model = "gpt-6.1-sol"\nmodel_reasoning_effort = "low"\n')
  env['CODEX_HOME']=str(home)
  argv=['codex','app-server','--listen','stdio://']
 else:
  config=d/'home';config.mkdir();creds=pathlib.Path.home()/'.claude/.credentials.json'
  if creds.exists():
   shutil.copyfile(creds,config/'.credentials.json');(config/'.credentials.json').chmod(0o600)
  env['CLAUDE_CONFIG_DIR']=str(config)
  argv=['claude','-p','--safe-mode','--input-format','stream-json','--output-format','stream-json','--verbose','--replay-user-messages','--no-session-persistence','--strict-mcp-config','--mcp-config','{"mcpServers":{}}','--tools','Bash','--allowedTools','Bash(python3 hold.py)','--permission-mode','dontAsk','--settings','{"crossSessionInbound":"accept"}','--effort','low']
 session_id=str(uuid.uuid4())
 c=Child(argv,d,env);report={'provider':provider,'root':str(d),'scope':'native protocol only; not Cadence end-to-end','status':'unknown'}
 try:
  if provider=='pi':
   c.send({'id':'initial','type':'prompt','message':PROMPT});r=c.wait(lambda e:e.get('id')=='initial')
   if not r.get('success'):raise RuntimeError(str(r))
  elif provider=='codex':
   c.request(1,'initialize',{'clientInfo':{'name':'cad1015-dogfood','version':'0.1.0'}});c.send({'jsonrpc':'2.0','method':'initialized'})
   thread=c.request(2,'thread/start',{'cwd':str(d),'approvalPolicy':'never','sandbox':'workspace-write'})['thread']['id']
   turn=c.request(3,'turn/start',{'threadId':thread,'input':[{'type':'text','text':PROMPT}],'clientUserMessageId':'cad1015-initial-'+str(uuid.uuid4())})['turn']['id']
   report.update(thread_id=thread,turn_id=turn)
  else:
   c.send({'type':'user','message':{'role':'user','content':PROMPT},'parent_tool_use_id':None,'session_id':session_id})
  busy(c,provider)
  deadline=time.monotonic()+5
  while not (d/'busy').exists():
   if time.monotonic()>deadline:raise RuntimeError('tool-start event did not produce the owned busy marker')
   time.sleep(.05)
  report['busy_tool_observed']=True
  if provider=='pi':
   c.send({'id':'amendment','type':'steer','message':AMEND});r=c.wait(lambda e:e.get('id')=='amendment',10)
   report['amendment_ack']=r
   if not r.get('success'):raise RuntimeError('Pi rejected steer')
  elif provider=='codex':
   c.send({'jsonrpc':'2.0','id':4,'method':'turn/steer','params':{'threadId':thread,'expectedTurnId':'stale-cad1015','input':[{'type':'text','text':'Do not execute this stale request.'}]}})
   stale=c.wait(lambda e:e.get('id')==4,10);report['stale_turn_rejected']='error' in stale
   if not report['stale_turn_rejected']:raise RuntimeError('stale expectedTurnId unexpectedly accepted')
   report['amendment_ack']=c.request(5,'turn/steer',{'threadId':thread,'expectedTurnId':turn,'input':[{'type':'text','text':AMEND}]})
  else:
   # This probe deliberately tests managed stream-json input, not direct mailbox-file writes.
   c.send({'type':'user','message':{'role':'user','content':AMEND},'parent_tool_use_id':None,'session_id':session_id})
   report['amendment_ack']='sent on owned stream; acceptance timing assessed from output'
  report['no_completion_before_release']=not any(e.get('type') in ('agent_settled','result') or e.get('method')=='turn/completed' for e in c.seen)
  (d/'release').write_text('release')
  if provider=='pi':c.wait(lambda e:e.get('type')=='agent_settled')
  elif provider=='codex':c.wait(lambda e:e.get('method')=='turn/completed' and e.get('params',{}).get('turn',{}).get('id')==turn)
  else:c.wait(lambda e:e.get('type')=='result')
  initial=texts(c.seen,provider);report['initial_outputs']=initial
  report['amendment_observed_in_initial_run']=any('AMENDED_CAD1015' in t for t in initial)
  report['first_result_baseline_only']=any(t.strip()=='BASELINE' for t in initial)
  if provider=='claude' and not report['amendment_observed_in_initial_run']:
   c.wait(lambda e:e.get('type')=='result',30)
   report['outputs_after_second_result']=texts(c.seen,provider)
   report['amendment_processed_as_followup']=any('AMENDED_CAD1015' in t for t in report['outputs_after_second_result'])
  if provider=='pi':
   # Negative boundary probe: native steer has no expectedTurnId guard.
   c.send({'id':'idle-steer','type':'steer','message':'For your next reply include IDLE_CAD1015 after the requested token. Do not use tools.'})
   report['idle_steer_ack']=c.wait(lambda e:e.get('id')=='idle-steer',10)
   start=len(c.seen);c.send({'id':'followup','type':'prompt','message':FOLLOW});r=c.wait(lambda e:e.get('id')=='followup')
   if not r.get('success'):raise RuntimeError(str(r))
   c.wait(lambda e:e.get('type')=='agent_settled');report['followup_outputs']=texts(c.seen[start:],provider)
   report['idle_steer_leaked_into_next_run']=any('IDLE_CAD1015' in t for t in report['followup_outputs'])
  elif provider=='codex':
   start=len(c.seen);t=c.request(6,'turn/start',{'threadId':thread,'input':[{'type':'text','text':FOLLOW}],'clientUserMessageId':'cad1015-followup-'+str(uuid.uuid4())})['turn']['id']
   c.wait(lambda e:e.get('method')=='turn/completed' and e.get('params',{}).get('turn',{}).get('id')==t);report['followup_outputs']=texts(c.seen[start:],provider)
  if provider in ('pi','codex'):
   assert any('FOLLOWUP_CAD1015' in t for t in report['followup_outputs']), 'follow-up not processed'
  assert report['no_completion_before_release'], 'completion observed while hold remained blocked'
  assert report['amendment_observed_in_initial_run'], 'amendment did not join original run'
  report['status']='observed'
 except Exception as e:
  report['status']='blocked';report['error']=str(e)
 finally:
  (d/'release').write_text('cleanup-release');c.stop()
  for name in ('auth.json','.credentials.json'):
   (d/'home'/name).unlink(missing_ok=True)
  report['process_exit']=c.p.returncode
  report['event_types']=[e.get('method',e.get('type','non_json')) for e in c.seen]
  # Do not record raw config/auth, environment, stderr or tool I/O.
  if c.errors:report['stderr_line_count']=len(c.errors)
  (root/f'{provider}-report.json').write_text(json.dumps(report,indent=2)+'\n')
 return report

if __name__=='__main__':
 if os.environ.get('CAD1015_DOGFOOD') != '1':
  raise SystemExit('Opt-in required: CAD1015_DOGFOOD=1 (starts owned real model session)')
 ap=argparse.ArgumentParser();ap.add_argument('provider',choices=['pi','codex','claude']);ap.add_argument('--root');a=ap.parse_args()
 root=pathlib.Path(a.root) if a.root else pathlib.Path(tempfile.mkdtemp(prefix='c1015-'));root.mkdir(parents=True,exist_ok=True)
 result=probe(a.provider,root);print(json.dumps(result,indent=2));raise SystemExit(0 if result['status']=='observed' else 1)
