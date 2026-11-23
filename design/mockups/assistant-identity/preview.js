'use strict';
// All conversations, identity updates and coordinator queries below are simulated in memory.
// No fetch, real authentication, provider invocation, dispatch or profile persistence is used.
const $ = (s) => document.querySelector(s);
const icon = (name, cls = '') => `<svg class="ico ${cls}" aria-hidden="true"><use href="#i-${name}"/></svg>`;
const escapeHTML = (value) => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
// Canonical AgentAvatar.tsx artwork, exported with ReactDOMServer; no new avatar design.
// Same precedence as agentLook: explicitly pinned look, then Master robot, then stable alias hash.
const avatarLooks=['builder','guard','bookkeeper','content','designer','reception','operations','support','tester','robot','custom'];
let personalLook=null,coordinatorLook=null,avatarPickerOpen=false;
function currentLook(){const pinned=state.workspace?coordinatorLook:personalLook;if(pinned)return pinned;if(state.workspace)return 'robot';let hash=0;for(const ch of 'personal-fieldwork-avery')hash=(hash*31+ch.charCodeAt(0))>>>0;return avatarLooks[hash%avatarLooks.length];}
function avatarImage(){return `<img class="avatar-face" src="avatars/${currentLook()}.svg" alt="" aria-hidden="true" data-look="${currentLook()}">`;}
function renderAvatarPicker(){
 let section=$('#avatarChoice');if(!section){section=document.createElement('section');section.id='avatarChoice';section.className='avatar-choice';$('.profile-inner').insertBefore(section,$('.profile-tip'));}
 section.innerHTML=`<div class="avatar-choice-heading"><span>${L(['Avatar','頭像'])}</span><button class="lnk" type="button" aria-expanded="${avatarPickerOpen}">${L(['Change','更換'])}</button></div><p>${L(['Assigned once from the existing set. Stays the same when you rename me.','從現有頭像中固定選擇，更名後仍保留同一個頭像。'])}</p><div class="avatar-options" role="group" aria-label="${L(['Choose an avatar','選擇頭像'])}" ${avatarPickerOpen?'':'hidden'}>${avatarLooks.map(look=>`<button type="button" data-avatar="${look}" aria-label="${look}" title="${look}" aria-pressed="${look===currentLook()}"><img src="avatars/${look}.svg" alt=""></button>`).join('')}</div><small ${avatarPickerOpen?'':'hidden'}>${L(['Appearance only. Roles and permissions do not change.','只改變外觀，不會改變角色或權限。'])}</small>`;
 section.querySelector('.avatar-choice-heading button').addEventListener('click',()=>{avatarPickerOpen=!avatarPickerOpen;renderAvatarPicker();$('#avatarChoice .avatar-choice-heading button').focus();});
 section.querySelectorAll('[data-avatar]').forEach(button=>button.addEventListener('click',()=>{if(state.workspace)coordinatorLook=button.dataset.avatar;else personalLook=button.dataset.avatar;render();document.querySelector(`[data-avatar="${currentLook()}"]`).focus();}));
}
let lang = 'en', phase = 1, scene = 'first', profileOpen = false, generation = 0, toastTimer;
let state;
const copy = {
  review:['DESIGN PREVIEW','設計預覽'],reviewTitle:['An assistant that feels like yours.','一位真正屬於你的助理。'],phase1:['01 Identity','01 名稱與個性'],phase2:['02 Coordination','02 協作'],sceneLabel:['Preview scene','預覽情境'],simulation:['Interactive mockup · sample data · changes reset on reload','互動模型 · 示範資料 · 重新載入後重設'],workspace:['Workspace','工作區'],home:['Home','首頁'],myWork:['My work','我的工作'],threads:['Threads','對話串'],projects:['Projects','專案'],knowledge:['Knowledge','知識庫'],apps:['APPS','應用程式'],agents:['Agents','代理程式'],settings:['Settings','設定'],owner:['Workspace owner','工作區擁有者'],aboutTitle:['About your assistant','關於你的助理'],displayName:['Display name','顯示名稱'],personality:['Personality','溝通風格與個性'],callsYou:['Calls you','如何稱呼你'],belongsTo:['Belongs to','屬於'],changeInChat:['Change it in conversation.','在對話中隨時調整。'],changeTip:['Ask me to change my name or how I communicate. Your conversations stay with you.','直接告訴我想更改的名稱或溝通風格。你的對話紀錄會完整保留。'],editWorkspace:['Edit workspace identity','編輯工作區助理資料'],springCampaign:['Spring collection','春季系列'],contextOnly:['This campaign context','僅限此行銷活動情境'],campaigns:['CAMPAIGNS','行銷活動'],campaignDesc:['A thoughtful introduction to the new collection.','讓新系列以恰到好處的方式與顧客見面。'],draft:['Draft','草稿'],overview:['Overview','概覽'],audience:['Audience','受眾'],email:['Email','電郵'],appNote:['Chat uses your access to this campaign. Approval is still required before sending.','對話依照你對此行銷活動的存取權運作。寄送前仍需核准。'],keyboardHint:['Enter to send · Shift + Enter for a new line','Enter 傳送 · Shift + Enter 換行'],operatorNote:['Operator-only edit · applies to the workspace coordinator, not your personal assistant.','僅限操作員編輯 · 變更工作區協調助理，不會更改你的私人助理。'],operatorGuard:['Changing identity never changes the coordinator’s permissions.','更改名稱或個性不會改變協調助理的權限。'],cancel:['Cancel','取消'],saveIdentity:['Save identity','儲存資料'],fallback:['Assistant','助理'],workspaceAssistant:['Workspace assistant','工作區助理'],private:['Private to you','僅你可見'],personalScope:['Your personal assistant · Fieldwork','你的私人助理 · Fieldwork'],workspaceScope:['Workspace coordinator · Shared workspace identity','工作區協調助理 · 工作區共用身分'],notSet:['Not named yet','尚未命名'],defaultTone:['Clear, practical, and friendly','清楚、務實、友善'],personalOwner:['You · Fieldwork','你 · Fieldwork'],workspaceOwner:['Fieldwork workspace','Fieldwork 工作區'],personalFoot:['Your assistant’s identity belongs to you in this workspace. Naming never changes access or permissions.','這位助理的個人設定僅屬於你在此工作區的身分。更名不會改變存取權限。'],workspaceFoot:['This profile belongs to the workspace, not its current owner. Only an operator can edit it.','此設定屬於工作區，而不是目前的擁有者。僅操作員可以編輯。'],workspaceTip:['Workspace-owned identity.','屬於工作區的身分。'],workspaceTipText:['Use the operator edit control to update the coordinator. Your personal name and preferences stay separate.','使用操作員編輯功能更新協調助理。你的私人助理名稱及偏好不會受影響。'],you:['You','你'],today:['TODAY','今天'],profileSaved:['Profile saved','個人設定已儲存'],setupOptional:['Personalization is optional','個人化設定為選填'],setupLater:['Personalization deferred','稍後再進行個人化'],setupSkipped:['Personalization skipped','已略過個人化'],sampleDate:['10:42','10:42'],retry:['Try again','重試'],saveError:['Your name wasn’t changed. Your previous identity and conversation are intact.','名稱尚未更改。原有名稱與對話紀錄均已保留。'],readOnly:['Read-only query','唯讀查詢'],queryLoading:['Checking with the workspace coordinator','正在向工作區協調助理查詢'],queryDone:['Workspace coordinator consulted','已向工作區協調助理查詢'],queryUnavailable:['Workspace coordinator unavailable','工作區協調助理暫時無法使用'],queryScope:['Scope: Spring collection project','範圍：春季系列專案'],queryAppScope:['Scope: this campaign only','範圍：僅限此行銷活動'],queryAccess:['Uses your current access. No shared chat history is included.','依照你目前的權限查詢，不會帶入共用對話紀錄。'],queryEffect:['No actions, approvals, or dispatch.','不會執行操作、核准或派工。'],sourceTitle:['Campaign review','行銷活動審核'],sourceMeta:['Spring collection · Work item FW-27','春季系列 · 工作項目 FW-27'],sourceDetail:['FW-27 · Campaign review\nDraft prepared by Elena. Waiting for the product team to confirm the launch wording. This sample work item is visible to Avery.','FW-27 · 行銷活動審核\nElena 已完成草稿，目前等候產品團隊確認上市文案。此示範工作項目在 Avery 的存取範圍內。'],tryName:['Try “Call yourself Nova.” Then agree on a communication style.','試試「以後叫你 Nova」。接著再選擇溝通風格。'],renameNote:['Try “Call yourself Atlas.” The same assistant and history remain.','試試「以後叫你 Atlas」。助理身分與對話紀錄不變。'],toneNote:['Try “Be concise and formal.” Name and history stay unchanged.','試試「請簡潔而正式地回覆」。名稱及對話紀錄不變。'],workNote:['Useful work comes first. Personalization never blocks the answer.','先完成使用者的需求，個人化設定不會阻擋回覆。'],namedNote:['Identity is saved in this simulated session. Open About to inspect it.','此示範工作階段已儲存個人設定。開啟「關於」查看。'],failureNote:['Simulate a refused rename: send “Call yourself Atlas”, then retry explicitly.','模擬更名失敗：傳送「以後叫你 Atlas」，然後手動重試。'],workspaceNote:['Phase 1 · existing Master-backed screen. Workspace identity, operator-only editing.','第一階段 · 現有 Master 對話畫面，名稱屬於工作區，僅限操作員編輯。'],queryNote:['Phase 2 · user’s assistant asks Master through a scoped, read-only exchange.','第二階段 · 私人助理透過限縮範圍的唯讀管道向 Master 查詢。'],appSceneNote:['Phase 2 · same personal assistant, separate CRM context and conversation.','第二階段 · 同一位私人助理，獨立的 CRM 情境與對話。'],unavailableNote:['Phase 2 · unavailable query is explicit. No guessed answer or fallback to shared chat.','第二階段 · 明確顯示查詢無法完成，不猜測答案或切換到共用對話。']
};
const t = key => copy[key]?.[lang === 'en' ? 0 : 1] ?? key;
const L = value => Array.isArray(value) ? value[lang === 'en' ? 0 : 1] : value;
const scenes = {1:[['first',['First conversation','第一次對話']],['named',['Named & personalized','已命名與設定個性']],['rename',['Rename later','稍後更改名稱']],['personality',['Change personality','調整個性']],['work',['Work comes first','先完成工作']],['failure',['Rename not saved','更名儲存失敗']],['workspace',['Workspace identity','工作區助理身分']]],2:[['query',['Home → coordinator query','首頁 → 協調助理查詢']],['app',['CRM → same assistant','CRM → 同一位助理']],['unavailable',['Coordinator unavailable','協調助理無法使用']]]};
const message = (role, en, zh, extra={}) => ({role,text:[en,zh],...extra});
function effectiveName(){ return state.name || t(state.workspace ? 'workspaceAssistant' : 'fallback'); }
function initialMessages(which){
 const hello=message('user','Hello.','你好。');
 const first=message('assistant','Hi, Avery. I’m your personal assistant in Fieldwork. What would you like to call me?','你好，Avery。我是你在 Fieldwork 的私人助理。你希望怎麼稱呼我？');
 if(which==='first')return [hello,first];
 if(which==='named')return [hello,first,message('user','Call yourself Nova.','以後叫你 Nova。'),message('assistant','Nova it is. How would you like me to communicate?','那就叫我 Nova。你喜歡我用什麼風格與你溝通？'),message('user','Concise, friendly, and practical.','簡潔、友善、務實。'),message('assistant','Got it. I’ll keep things clear and useful, and say when I’m uncertain. You can change my name or style anytime.','明白。我會簡潔實用地回覆，不確定時也會直說。之後隨時可以更改我的名稱或風格。',{saved:true})];
 if(which==='rename'||which==='failure')return [message('user','Thanks for helping with the campaign brief. I’d like to change your name.','謝謝你協助整理行銷活動摘要。我想更改你的名稱。'),message('assistant','Of course. What would you like to call me instead? Your conversations and preferences will stay as they are.','當然可以。你想改成什麼名字？原有對話紀錄及偏好設定都會保留。')];
 if(which==='personality')return [message('user','Nova, can we adjust your communication style?','Nova，我可以調整你的回覆風格嗎？'),message('assistant','Absolutely. Tell me how you’d like me to communicate.','當然。告訴我你希望我怎麼回覆就可以了。')];
 if(which==='work')return [message('user','Turn this brief into a short checklist: finalize the campaign copy, confirm the audience, and get approval before sending.','將這份摘要整理成簡短清單：完成行銷文案、確認受眾，並在寄送前取得核准。'),message('assistant','Here’s your checklist:\n\n1. Finalize the campaign copy.\n2. Confirm the intended audience.\n3. Obtain approval before sending.\n\nNothing has been sent. Whenever it suits you, what would you like to call me?','工作清單如下：\n\n1. 完成行銷活動文案。\n2. 確認目標受眾。\n3. 寄送前取得核准。\n\n尚未寄送任何內容。方便時，你希望怎麼稱呼我？')];
 if(which==='workspace')return [message('user','What is your role in this workspace?','你在這個工作區負責什麼？'),message('assistant','I coordinate Fieldwork’s approved work, route questions, and track shared progress. My identity belongs to this workspace—not to any one person.\n\nAn operator can update my workspace name and communication style. Your personal assistant keeps its own name and private preferences.','我負責協調 Fieldwork 已核准的工作、轉達問題，並追蹤共用工作進度。我的身分屬於工作區，而不是任何一位成員。\n\n操作員可以更改我的工作區名稱與溝通風格。你的私人助理則保有獨立的名稱及私人偏好。')];
 if(which==='app')return [message('assistant','I’m here with the Spring collection campaign open. This conversation is separate from Home, but it’s still me—Nova. What would you like to check?','我已開啟春季系列行銷活動。這段對話與首頁分開，但仍由我 Nova 協助你。你想查看什麼？')];
 return [message('assistant','I can help you check the work you have access to. For shared progress, I can ask the workspace coordinator without opening its shared conversation.','我可以協助查看你有權存取的工作。需要確認共用進度時，我會向工作區協調助理查詢，不會開啟它的共用對話。')];
}
function loadScene(which){
 generation++; scene=which; profileOpen=false;
 const named=!['first','work','workspace'].includes(which);
 state={name:which==='workspace'?'Fieldwork assistant':named?'Nova':null,tone:named?['Concise, friendly, and practical','簡潔、友善、務實']:null,userName:'Avery',workspace:which==='workspace',step:['first','work'].includes(which)?'name':['rename','failure'].includes(which)?'rename':which==='personality'?'tone':'done',status:named?'complete':'pending',messages:initialMessages(which),failure:which==='failure',unavailable:which==='unavailable',busy:false,error:null,pendingName:null,appTab:'overview',lastChange:false};
 state.mobilePanel='assistant';
 $('#message').value='';$('#inlineError').hidden=true;render();
}
function sceneOptions(){ const select=$('#scene'); select.replaceChildren(); scenes[phase].forEach(([value,label])=>{const option=document.createElement('option');option.value=value;option.textContent=L(label);select.append(option);});select.value=scene; }
function render(){
 document.documentElement.lang=lang==='en'?'en':'zh-Hant';
 document.querySelectorAll('[data-i18n]').forEach(el=>el.textContent=t(el.dataset.i18n));
 $('#phase1').setAttribute('aria-pressed',String(phase===1));$('#phase2').setAttribute('aria-pressed',String(phase===2));
 $('#locale').textContent=lang==='en'?'繁中':'EN';$('#locale').setAttribute('aria-label',lang==='en'?'Switch to Traditional Chinese':'Switch to English');
 $('#theme').setAttribute('aria-label',lang==='en'?'Toggle color theme':'切換色彩主題');$('#reset').setAttribute('aria-label',lang==='en'?'Reset preview':'重設預覽');
 sceneOptions();$('#sceneNote').textContent=t(({first:'tryName',named:'namedNote',rename:'renameNote',personality:'toneNote',work:'workNote',failure:'failureNote',workspace:'workspaceNote',query:'queryNote',app:'appSceneNote',unavailable:'unavailableNote'})[scene]);
 const name=effectiveName(),app=scene==='app';
 $('.assistant-avatar').innerHTML=avatarImage();$('.profile-symbol').innerHTML=avatarImage();
 $('#mobileAppTabs').hidden=!app;$('#mobileAssistant').textContent=name;$('#mobileWorkspace').textContent=t('springCampaign');$('#mobileAssistant').setAttribute('aria-pressed',String(state.mobilePanel==='assistant'));$('#mobileWorkspace').setAttribute('aria-pressed',String(state.mobilePanel==='workspace'));$('#productBody').classList.toggle('mobile-app-workspace',app&&state.mobilePanel==='workspace');
 $('#assistantName').textContent=name;$('#scopeText').textContent=t(state.workspace?'workspaceScope':'personalScope');
 $('#topScope').textContent=t(state.workspace?'workspaceAssistant':'private');$('#crumb').textContent=app?'CRM / '+t('springCampaign'):t('home');
 $('#composerScope').textContent=app?t('contextOnly'):t(state.workspace?'workspaceOwner':'private');
 $('#composerLabel').textContent=lang==='en'?`Message ${name}`:`傳送訊息給 ${name}`;$('#message').placeholder=lang==='en'?`Message ${name}…`:`傳送訊息給 ${name}…`;
 $('#newChat').setAttribute('aria-label',lang==='en'?'New conversation':'新增對話');$('#about').setAttribute('aria-label',lang==='en'?'About your assistant':'關於你的助理');
 $('#send').setAttribute('aria-label',lang==='en'?'Send message':'傳送訊息');$('#closeProfile').setAttribute('aria-label',lang==='en'?'Close assistant details':'關閉助理資料');
 $('#profileStatus').textContent=state.workspace?t('workspaceAssistant'):state.status==='complete'?t('profileSaved'):state.status==='deferred'?t('setupLater'):state.status==='skipped'?t('setupSkipped'):t('setupOptional');
 $('#about').setAttribute('aria-expanded',String(profileOpen));$('#profilePanel').hidden=!profileOpen;
 $('#productBody').classList.toggle('has-profile',profileOpen);$('#productBody').classList.toggle('app-view',app);$('#crmPanel').hidden=!app||profileOpen;$('#contextLine').hidden=!app;
 document.querySelectorAll('[data-nav]').forEach(el=>{const current=el.dataset.nav===(app?'crm':'home');if(current)el.setAttribute('aria-current','page');else el.removeAttribute('aria-current');});
 renderProfile();renderMessages();renderError();renderApp();updateSend();
}
function renderProfile(){
 const name=effectiveName();$('#profileName').textContent=name;$('#profileOwnership').textContent=t(state.workspace?'workspaceScope':'personalScope');$('#detailName').textContent=state.name||t('notSet');$('#detailTone').textContent=state.tone?L(state.tone):t('defaultTone');$('#detailOwner').textContent=t(state.workspace?'workspaceOwner':'personalOwner');
 $('.profile-inner dl>div:nth-child(3) dd').textContent=state.workspace?'—':state.userName;
 $('.profile-tip strong').textContent=t(state.workspace?'workspaceTip':'changeInChat');$('.profile-tip p').textContent=t(state.workspace?'workspaceTipText':'changeTip');$('#profileFootnote').textContent=t(state.workspace?'workspaceFoot':'personalFoot');$('#editWorkspace').hidden=!state.workspace;
 renderAvatarPicker();
}
function renderMessages(){
 const container=$('#messages');container.replaceChildren();const date=document.createElement('div');date.className='date-line';date.textContent=t('today');container.append(date);
 state.messages.forEach((m,index)=>{
  if(m.role==='event'){const event=document.createElement('div');event.className='profile-event';event.innerHTML=icon('check')+`<span>${escapeHTML(L(m.text))}</span>`;container.append(event);return;}
  const el=document.createElement('article');el.className=`message ${m.role}`;
  const label=m.role==='user'?t('you'):effectiveName();
  el.innerHTML=`<div class="message-meta">${m.role==='assistant'?avatarImage():''}<strong>${escapeHTML(label)}</strong><time>${index<4?'10:42':'10:43'}</time></div>`;
  if(m.query){const details=document.createElement('details');details.className='query-disclosure';details.innerHTML=`<summary>${icon(m.query==='loading'?'refresh':m.query==='unavailable'?'info':'check')}<span>${escapeHTML(t(m.query==='loading'?'queryLoading':m.query==='unavailable'?'queryUnavailable':'queryDone'))}</span>${icon('chevron')}</summary><div class="query-detail"><strong>${escapeHTML(t('readOnly'))}</strong><span>${escapeHTML(t(scene==='app'?'queryAppScope':'queryScope'))}</span><span>${escapeHTML(t('queryAccess'))}</span><span>${escapeHTML(t('queryEffect'))}</span></div>`;el.append(details);}
  if(L(m.text)){const body=document.createElement('div');body.className='message-body';body.textContent=L(m.text);el.append(body);}
  if(m.source){const source=document.createElement('button');source.type='button';source.className='source-reference';source.setAttribute('aria-expanded',String(m.expanded||false));source.innerHTML=icon('folder')+`<span>${escapeHTML(t('sourceTitle'))}<small>${escapeHTML(t('sourceMeta'))}</small></span>`+icon('chevron');source.addEventListener('click',()=>{m.expanded=!m.expanded;renderMessages();});el.append(source);if(m.expanded){const reference=document.createElement('div');reference.className='reference-expanded';reference.textContent=t('sourceDetail');el.append(reference);}}
  if(m.saved){const footer=document.createElement('div');footer.className='message-footer';footer.innerHTML=icon('check')+`<span>${escapeHTML(t('profileSaved'))}</span>`;el.append(footer);}
  if(m.query==='unavailable'){const retry=document.createElement('button');retry.className='lnk';retry.textContent=t('retry');retry.addEventListener('click',()=>{state.unavailable=false;runQuery(m);});el.append(retry);}
  container.append(el);
 });
 const scroll=$('#conversationScroll');requestAnimationFrame(()=>{scroll.scrollTop=scroll.scrollHeight;});
}
function renderError(){const box=$('#inlineError');box.hidden=!state.error;if(!state.error)return;box.innerHTML=`<span>${escapeHTML(t('saveError'))}</span>`;const retry=document.createElement('button');retry.type='button';retry.textContent=t('retry');retry.addEventListener('click',()=>{const next=state.pendingName;state.error=null;state.failure=false;saveName(next);});box.append(retry);}
function updateSend(){ $('#send').disabled=state.busy||!$('#message').value.trim();$('#message').disabled=state.busy; }
function addReply(en,zh,extra={}){state.messages.push(message('assistant',en,zh,extra));render();}
function event(en,zh){state.messages.push(message('event',en,zh));}
function saveName(name){
 const old=effectiveName();state.name=name;state.error=null;state.lastChange=true;
 if(state.step==='name'){
  state.step='tone';event(`Name saved · ${name}`,`名稱已儲存 · ${name}`);addReply(`${name} it is. How would you like me to communicate?`, `那就叫我 ${name}。你希望我用什麼風格與你溝通？`);
 }else{
  state.step='done';event(`Name updated · ${old} → ${name} · Same assistant and history`,`名稱已更新 · ${old} → ${name} · 助理身分及對話紀錄不變`);
  addReply(`You can call me ${name} from now on. I’ve kept your conversations and other preferences unchanged.`,`之後就叫我 ${name}。原有對話紀錄與其他偏好設定都沒有改變。`,{saved:true});
 }
}
function runQuery(existing){
 const token=generation;state.busy=true;let target=existing;
 if(!target){target=message('assistant','','',{query:'loading'});state.messages.push(target);}else{target.query='loading';target.text=['',''];}
 render();
 setTimeout(()=>{
  if(generation!==token)return;state.busy=false;
  if(state.unavailable){target.query='unavailable';target.text=['I couldn’t verify the current blockers because the workspace coordinator is unavailable. No work was changed. You can try the query again.','工作區協調助理目前無法使用，因此我無法確認最新的阻擋事項。尚未變更任何工作，你可以稍後重試查詢。'];}
  else{target.query='done';target.source=true;target.text=scene==='app'?['This campaign is waiting for the product team to review the launch wording. Elena’s draft is ready.\n\nThe next step is the campaign review—not sending the email. I haven’t changed or approved anything.','此行銷活動目前等候產品團隊審核上市文案。Elena 的草稿已準備完成。\n\n下一步是完成行銷活動審核，而不是寄送電郵。我尚未變更或核准任何內容。']:['The Spring collection launch is waiting on the campaign review. Elena has finished the draft; the product team still needs to confirm the launch wording.\n\nThat is the next dependency to resolve. Nothing has been dispatched or approved.','春季系列上市目前卡在行銷活動審核。Elena 已完成草稿，產品團隊仍需確認上市文案。\n\n這是下一個需要解決的相依事項。尚未進行任何派工或核准。'];}
  render();$('#message').focus();
 },750);
}
function handleSend(text){
 if(state.busy||!text.trim())return;const raw=text.trim();state.error=null;state.messages.push(message('user',raw,raw));$('#message').value='';
 if(state.workspace){addReply('My name and style belong to the workspace. An operator can update them using “Edit workspace identity” in About. This chat cannot change my permissions or private user profiles.','我的名稱與風格屬於工作區。操作員可在「關於」中使用「編輯工作區助理資料」進行調整。這段對話不會更改我的權限或任何私人助理資料。');return;}
 if(/^(later|not now|稍後|以後再說|稍後再說|遲啲)[.!。！]?$/i.test(raw)){state.status='deferred';state.step='done';addReply('Of course. We can do that later. What would you like help with?','沒問題，稍後再設定。現在需要我協助什麼？');return;}
 if(/^(skip|skip setup|略過|跳過|略過設定)[.!。！]?$/i.test(raw)){state.status='skipped';state.step='done';addReply('No problem. You can ask me to change my name or style whenever you want.','沒問題。之後隨時可以請我更改名稱或風格。');return;}
 if(/resume.*(setup|personal)|繼續設定|恢復設定|開始設定/i.test(raw)){state.status='pending';state.step=state.name?'tone':'name';addReply(state.name?'How would you like me to communicate?':'What would you like to call me?',state.name?'你希望我用什麼風格與你溝通？':'你希望怎麼稱呼我？');return;}
 if(/default name|reset.*name|預設名稱|恢復.*名稱/i.test(raw)){const old=effectiveName();state.name=null;state.step='done';event(`Name cleared · ${old} → Assistant`,`已清除自訂名稱 · ${old} → 助理`);addReply('I’ll use Assistant as my display name again. Your history and other preferences are unchanged.','我會重新使用「助理」作為顯示名稱。對話紀錄及其他偏好不變。',{saved:true});return;}
 const userMatch=raw.match(/^(?:call me|my name is)\s+(.+?)\s*[.!。！]?$/i)||raw.match(/^(?:叫我|稱呼我為?|我叫)\s*(.+?)\s*[。！]?$/);
 if(userMatch){state.userName=userMatch[1];addReply(`I’ll call you ${state.userName}. My assistant name is still ${effectiveName()}.`,`我會稱呼你為 ${state.userName}。我的助理名稱仍是 ${effectiveName()}。`,{saved:true});return;}
 let nameMatch=raw.match(/^(?:call yourself|your name is|rename yourself to|you can be called|i(?:'ll| will) call you)\s+(.+?)\s*[.!。！]?$/i)||raw.match(/^(?:以後叫你|以後你叫|你叫|你的名字是|叫你|改名為?|請叫做)\s*(.+?)\s*[。！]?$/);
 const isNameStage=['name','rename'].includes(state.step);
 const plainName=isNameStage&&raw.length<=80&&!/[?？\n]/.test(raw)&&!/(summari|campaign|checklist|hello|hi\b|風格|整理|你好|回覆)/i.test(raw);
 if(nameMatch||plainName){const next=(nameMatch?nameMatch[1]:raw).trim();if(!next){render();return;}if(next.length>80){addReply('Please choose a shorter display name for this preview (up to 80 characters).','此預覽請使用較短的顯示名稱（最多 80 字）。');return;}if(state.failure){state.error='rename';state.pendingName=next;render();return;}saveName(next);return;}
 if(state.step==='tone'||/be (more |less )?(concise|formal|friendly|warm)|communicat|personality|tone|簡潔|正式|友善|個性|風格/i.test(raw)){
  state.tone=[raw,raw];state.step='done';state.status='complete';event('Communication style updated','溝通風格已更新');addReply(`Understood. I’ll use that communication style. You can change it again anytime. My name is still ${effectiveName()}.`,`明白，我會依照這個風格回覆。之後隨時可以再調整。我的名稱仍是 ${effectiveName()}。`,{saved:true});return;
 }
 if(phase===2){runQuery();return;}
 if(/checklist|summari|整理|摘要|清單/i.test(raw)){addReply('Here is the short checklist for this sample campaign:\n\n1. Finalize the draft.\n2. Confirm the audience.\n3. Obtain approval before sending.\n\nNothing has been sent.','這個示範行銷活動的簡短清單如下：\n\n1. 完成草稿。\n2. 確認受眾。\n3. 寄送前取得核准。\n\n尚未寄送任何內容。');return;}
 addReply('This design preview demonstrates naming, communication preferences, and later updates. You can say “Call yourself Nova” or “Be concise and friendly.” Workspace queries are available in the Phase 2 preview.','此設計預覽示範命名、溝通偏好及日後修改。你可以說「以後叫你 Nova」或「請簡潔而友善地回覆」。工作區查詢可在第二階段預覽中體驗。');
}
function renderApp(){
 const content=$('#appContent');if(!content)return;
 document.querySelectorAll('[data-app-tab]').forEach(el=>el.toggleAttribute('data-on',el.dataset.appTab===state.appTab));
 const esc=escapeHTML;
 if(state.appTab==='email'){content.innerHTML=`<div class="email-preview"><span class="slabel">FIELDWORK / ${esc(t('draft'))}</span><h3>${L(['Made for a fresh start.','為全新的開始而設。'])}</h3><p>${L(['Meet the Spring collection. Considered details, useful essentials, and a little room for something new.','認識春季系列：細緻巧思、實用日常，還有迎接新事物的空間。'])}</p></div>`;return;}
 const rows=state.appTab==='audience'?[[['Audience','受眾'],['Newsletter subscribers','電子報訂閱者']],[['Consent','同意狀態'],['Subscribed contacts only','僅限已同意訂閱的聯絡人']],[['Exclusions','排除條件'],['Unsubscribed and suppressed contacts','已取消訂閱及禁止寄送的聯絡人']]]:[[['Status','狀態'],`<span class="status">${L(['Awaiting review','等候審核'])}</span>`],[['Draft owner','草稿負責人'],'Elena Park'],[['Next step','下一步'],['Product team reviews launch wording','產品團隊審核上市文案']],[['Sending','寄送'],['Not approved · nothing sent','尚未核准 · 尚未寄送']]];
 content.innerHTML=`<section class="campaign-section"><header><strong>${esc(L(state.appTab==='audience'?['Audience rules','受眾規則']:['Campaign details','行銷活動資料']))}</strong>${icon('info')}</header><dl>${rows.map(([label,value])=>`<div><dt>${esc(L(label))}</dt><dd>${typeof value==='string'&&value.startsWith('<span')?value:esc(L(value))}</dd></div>`).join('')}</dl></section><section class="campaign-review">${icon('chat')}<div><h3>${L(['A review before the next step','下一步之前，先完成審核'])}</h3><p>${L(['The draft is ready for the product team. Ask your assistant what is blocking progress or what needs your attention.','草稿已準備好交由產品團隊審核。你可以詢問助理目前卡在哪裡，或有哪些事項需要你處理。'])}</p></div></section>`;
}
function showToast(text){clearTimeout(toastTimer);$('#toast').textContent=text;$('#toast').hidden=false;toastTimer=setTimeout(()=>$('#toast').hidden=true,2800);}
function closeNav(){$('#sidebar').classList.remove('open');$('#mobileScrim').hidden=true;$('#mobileMenu').setAttribute('aria-expanded','false');}
$('#phase1').addEventListener('click',()=>{phase=1;loadScene('first');});$('#phase2').addEventListener('click',()=>{phase=2;loadScene('query');});
$('#scene').addEventListener('change',e=>loadScene(e.target.value));$('#locale').addEventListener('click',()=>{lang=lang==='en'?'zh':'en';render();});
$('#theme').addEventListener('click',()=>{const theme=document.documentElement.dataset.theme==='dark'?'light':'dark';document.documentElement.dataset.theme=theme;try{localStorage.setItem('cadence-theme',theme);}catch{}});
$('#reset').addEventListener('click',()=>loadScene(scene));$('#message').addEventListener('input',updateSend);
$('#message').addEventListener('keydown',e=>{if(e.key==='Enter'&&!e.shiftKey&&!e.isComposing&&e.keyCode!==229){e.preventDefault();$('#composerForm').requestSubmit();}});
$('#composerForm').addEventListener('submit',e=>{e.preventDefault();handleSend($('#message').value);});
$('#about').addEventListener('click',()=>{profileOpen=!profileOpen;render();if(profileOpen)$('#closeProfile').focus();});$('#closeProfile').addEventListener('click',()=>{profileOpen=false;render();$('#about').focus();});
$('#newChat').addEventListener('click',()=>{generation++;state.busy=false;state.messages=[message('assistant',state.name?`Hi, Avery. It’s ${state.name}. What would you like help with?`:'What would you like help with?',state.name?`你好，Avery，我是 ${state.name}。需要我協助什麼？`:'需要我協助什麼？')];state.error=null;$('#message').value='';render();$('#message').focus();});
$('#mobileMenu').addEventListener('click',()=>{const open=$('#sidebar').classList.toggle('open');$('#mobileScrim').hidden=!open;$('#mobileMenu').setAttribute('aria-expanded',String(open));});$('#mobileScrim').addEventListener('click',closeNav);
$('#brandHome').addEventListener('click',e=>{e.preventDefault();closeNav();loadScene(phase===1?'first':'query');});
for(const button of document.querySelectorAll('[data-nav]')){button.addEventListener('click',()=>{closeNav();if(button.dataset.nav==='home')loadScene(phase===1?'first':'query');else if(button.dataset.nav==='crm'){phase=2;loadScene('app');}else if(button.dataset.nav==='settings'){profileOpen=true;render();}else showToast(L(['This preview focuses on assistant identity and coordination.','此預覽聚焦於助理身分與協作設計。']));});}
for(const button of document.querySelectorAll('[data-app-tab]')){button.addEventListener('click',()=>{state.appTab=button.dataset.appTab;renderApp();});}
$('#mobileAssistant').addEventListener('click',()=>{state.mobilePanel='assistant';render();});$('#mobileWorkspace').addEventListener('click',()=>{state.mobilePanel='workspace';render();});
$('#editWorkspace').addEventListener('click',()=>{$('#workspaceName').value=state.name||'';$('#workspaceTone').value=state.tone?L(state.tone):t('defaultTone');$('#workspaceDialog').showModal();});
function closeDialog(){$('#workspaceDialog').close();$('#editWorkspace').focus();}
$('#closeDialog').addEventListener('click',closeDialog);$('#cancelDialog').addEventListener('click',closeDialog);
$('#workspaceForm').addEventListener('submit',e=>{e.preventDefault();const name=$('#workspaceName').value.trim();if(!name){$('#workspaceName').focus();return;}state.name=name;const tone=$('#workspaceTone').value.trim();state.tone=[tone,tone];$('#workspaceDialog').close();event('Workspace identity updated by operator','操作員已更新工作區助理資料');render();showToast(L(['Workspace identity updated.','工作區助理資料已更新。']));$('#editWorkspace').focus();});
document.addEventListener('keydown',e=>{if(e.key==='Escape'){if($('#workspaceDialog').open)return;if(profileOpen){profileOpen=false;render();$('#about').focus();}closeNav();}});
new ResizeObserver(()=>document.documentElement.style.setProperty('--review-height',`${$('.reviewbar').getBoundingClientRect().height}px`)).observe($('.reviewbar'));
loadScene('first');
