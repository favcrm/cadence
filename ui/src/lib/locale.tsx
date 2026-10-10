import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { Meta } from "./types";

export type Locale = "en" | "zh-TW";
type Scope = "user" | "organization";
type Preferences = { organization_locale: Locale | null; user_locale: Locale | null };

const zhTW: Record<string, string> = {
  "Home": "首頁", "Overview": "總覽", "Projects": "專案", "Apps": "應用程式", "Agents": "代理程式", "Wiki": "知識庫", "Outbox": "寄件匣", "Settings": "設定", "Sign in": "登入", "Account": "帳戶", "Account menu": "帳戶選單", "Sign out": "登出", "Theme": "佈景主題", "System": "系統", "Light": "淺色", "Dark": "深色",
  "Language": "語言", "Follow organization": "依組織設定", "English": "English", "Traditional Chinese": "繁體中文", "Organization default": "組織預設語言", "Save": "儲存", "Saving…": "儲存中…", "Language preference saved.": "語言偏好已儲存。", "Could not save language preference.": "無法儲存語言偏好。",
  "All projects": "所有專案", "Search projects": "搜尋專案", "Search issues": "搜尋議題", "New issue": "新增議題", "Create issue": "建立議題", "Issue": "議題", "Issues": "議題", "Project": "專案", "Status": "狀態", "Priority": "優先級", "Owner": "負責人", "Title": "標題", "Description": "描述", "Tags": "標籤", "Save changes": "儲存變更", "Cancel": "取消", "Close": "關閉", "Open": "開啟", "Edit": "編輯", "Delete": "刪除", "Refresh": "重新整理", "Loading…": "載入中…", "Retry": "重試", "No projects yet": "尚無專案", "No issues found": "找不到議題", "No results": "沒有結果", "Try again": "再試一次", "Something went wrong": "發生錯誤",
  "To do": "待辦", "Doing": "進行中", "Review": "審查中", "Done": "已完成", "Blocked": "受阻", "Ready": "就緒", "Dropped": "已放棄", "Backlog": "待排程", "High": "高", "Medium": "中", "Low": "低", "Critical": "緊急",
  "Search": "搜尋", "Filter": "篩選", "Sort": "排序", "List": "清單", "Board": "看板", "Timeline": "時間軸", "Milestones": "里程碑", "Epics": "史詩議題", "New project": "新增專案", "Create project": "建立專案", "No issues": "尚無議題", "No description": "沒有描述", "Unassigned": "未指派", "Comments": "留言", "Activity": "活動", "Details": "詳細資料", "History": "歷程", "Links": "連結", "Blocked by": "受阻於", "Related": "相關", "Parent": "上層議題", "Add comment": "新增留言", "Write a comment…": "撰寫留言…", "Post comment": "發佈留言", "Update issue": "更新議題", "Move to": "移至", "Confirm": "確認", "Are you sure?": "確定要繼續嗎？",
  "Customers": "客戶", "Segments": "客群", "Campaigns": "行銷活動", "Conversation": "對話", "Assistant": "助理", "Workspace": "工作區", "Send": "傳送", "Send message": "傳送訊息", "Type a message…": "輸入訊息…", "New conversation": "新增對話", "Loading conversation…": "正在載入對話…", "No messages yet": "尚無訊息", "Something went wrong loading this conversation.": "載入此對話時發生錯誤。", "New customer": "新增客戶", "Add customer": "新增客戶", "Search customers": "搜尋客戶", "No customers yet": "尚無客戶", "No customers match your search.": "沒有符合搜尋條件的客戶。", "Customer details": "客戶資料", "Create segment": "建立客群", "New segment": "新增客群", "Search segments": "搜尋客群", "No segments yet": "尚無客群", "No campaigns yet": "尚無行銷活動", "Create campaign": "建立行銷活動", "New campaign": "新增行銷活動", "Name": "名稱", "Email": "電子郵件", "Phone": "電話", "Notes": "備註", "Loading customers…": "正在載入客戶…", "Loading segments…": "正在載入客群…", "Loading campaigns…": "正在載入行銷活動…", "Could not load customers.": "無法載入客戶。", "Could not load segments.": "無法載入客群。", "Could not load campaigns.": "無法載入行銷活動。", "Saved": "已儲存", "Changes saved": "變更已儲存", "Customer created": "已建立客戶", "Customer updated": "已更新客戶", "Segment created": "已建立客群", "Campaign created": "已建立行銷活動",
  "Skip to main content": "跳至主要內容", "Close menu": "關閉選單", "Open menu": "開啟選單", "Previous": "上一個", "Next": "下一個", "Remove": "移除", "Clear": "清除", "Apply": "套用", "Select all": "全選", "Loading": "載入中", "Error": "錯誤", "Saved successfully": "已成功儲存", "Required": "必填", "Not set": "尚未設定", "Primary": "主要導覽", "home": "首頁", "overview": "總覽", "projects": "專案", "apps": "應用程式", "agents": "代理程式", "wiki": "知識庫", "outbox": "寄件匣", "settings": "設定", "waiting for you": "項目待處理", "loading issues": "正在載入議題", "could not load projects": "無法載入專案", "not signed in · read only": "尚未登入 · 唯讀", "Read-only. Sign in to make changes": "唯讀。登入後即可變更", "Read only": "唯讀", "Sign in remotely": "遠端登入", "Or, on the host:": "或在主機上：", "Sign in to send messages and make decisions.": "登入即可傳送訊息並進行決策。", "Run this command on the host, then open the link it prints in this tab.": "在主機上執行此指令，然後在此分頁開啟輸出的連結。", "Account menu, signed in as": "帳戶選單，登入身分：", "Writes commit to the tracker as": "提交至追蹤器時顯示為", "This page needs a sign-in link, and it has none (or it was already used here).": "此頁需要登入連結，但目前沒有連結，或連結已在此使用。", "Signing this browser in…": "正在登入此瀏覽器…", "Signed in as the operator.": "已以操作員身分登入。", "Run": "請在主機上的終端機執行", "in your own shell on the host and open the new link within two minutes. Each link works once.": "並開啟新連結。連結需在兩分鐘內使用，且只能使用一次。", "All apps": "所有應用程式", "Context": "情境", "Choose a context": "選擇情境", "Earlier history is in Home": "較早的歷程請至首頁查看", "Contract preview (dev)": "合約預覽（開發）", "Exit contract preview": "離開合約預覽", "Panel": "面板", "Assistant chat": "助理對話", "workspace": "工作區", "Loading this app…": "正在載入此應用程式…", "App": "應用程式", "Sign in as the operator to inspect this installation.": "請以操作員身分登入以檢視此安裝。", "CRM setup is required before records open.": "請先完成 CRM 設定，才能開啟記錄。", "No context": "沒有情境", "Your workspace": "你的工作區", "Project overview": "專案總覽", "See where work is moving and choose a project to explore.": "查看工作進度並選擇要探索的專案。", "Current work, larger outcomes, and the next things to inspect.": "目前工作、較大型成果，以及接下來要檢視的項目。", "All issues": "所有議題", "Browse issues": "瀏覽議題", "Reading project work…": "正在讀取專案工作…", "Project work could not be loaded": "無法載入專案工作", "Issue counts exclude epics. In progress reflects tracker status; it does not mean an agent is currently running.": "議題數不包含史詩議題。「進行中」反映追蹤器狀態，不代表代理程式目前正在執行。", "Your projects": "你的專案", "in progress": "進行中", "in review": "審查中", "blocked": "受阻", "open issues": "個未結議題", "active epics": "個進行中史詩議題", "In review": "審查中", "Blocked work": "受阻工作", "Current focus": "目前重點", "No open work. This project is ready for its next goal.": "目前沒有未結工作。此專案已準備好設定下一個目標。", "No projects are available yet.": "目前沒有可用的專案。", "Current work": "目前工作", "Reviews and blockers first, followed by work in progress.": "優先顯示審查與受阻項目，其次是進行中的工作。", "No open issues in this project.": "此專案沒有未結議題。", "Active epics": "進行中的史詩議題", "See all": "查看全部", "No active epics yet.": "目前沒有進行中的史詩議題。", "Explore this project": "探索此專案", "Delivery dates, criteria, and evidence": "交付日期、標準與證據", "Repeatable work you can run": "可重複執行的工作流程", "Project guidance and reading order": "專案指引與閱讀順序", "issue": "議題", "lane": "工作區", "history": "歷程", "project": "專案", "status": "狀態", "priority": "優先級", "owner": "負責人", "component / tags": "元件／標籤", "checks": "檢查", "unassigned": "未指派", "Preview": "預覽", "No issues match this view.": "沒有符合此檢視條件的議題。", "done hidden — show": "個已完成議題已隱藏 — 顯示", "empty": "空白", "no pm dir": "找不到 PM 目錄", "Nothing at": "此處沒有資料：", "yet": "目前", "agents need attention · View details": "個代理程式需要處理 · 檢視詳細資料", "fenced": "已隔離", "outcomes are uncertain until an operator reconciles": "操作員完成核對前，結果仍不確定", "open Agents": "開啟代理程式", "epics are excluded — their status rolls up from the issues counted": "史詩議題不列入統計，其狀態由納入計算的議題彙總而成", "Project view": "專案檢視方式", "board": "看板", "list": "清單", "Search issues, owners or tags": "搜尋議題、負責人或標籤", "Team status": "團隊狀態", "running": "執行中", "queued": "排隊中", "need attention": "需要處理", "Runtime": "執行狀態", "parked": "已暫停", "inboxes": "收件匣", "inbox endpoints are mailboxes, not workers": "收件匣端點是信箱，不是工作者", "daemon unreachable": "無法連線至 daemon", "busy": "忙碌", "idle": "閒置", "stopped": "已停止", "agents without an exact issue binding remain global": "未綁定特定議題的代理程式會維持全域狀態", "global/unassigned": "全域／未指派", "loading issues — reading the tracker can take several seconds": "正在載入議題 — 讀取追蹤器可能需要數秒", "could not load issues": "無法載入議題", "the tracker has no issues yet": "追蹤器目前沒有議題", "the tracker has no issues yet — use New issue to create one": "追蹤器目前沒有議題 — 使用「新增議題」建立一項", "No epic": "沒有史詩議題", "of": "／", "children done (dropped excluded)": "個子議題已完成（不含已放棄）", "Task": "任務", "Question": "問題", "Feedback": "意見回饋", "Idea": "構想", "Bug": "錯誤", "Start work in": "開始在此工作：", "a project": "某個專案", "What needs to happen?": "需要完成什麼？", "Kind": "類型", "Files into": "將建立於", "this project": "此專案", "and triggers research + a plan draft.": "並啟動研究與計畫草稿。", "Files into the cadence project for triage.": "將建立於 cadence 專案以進行分類。", "optional": "選填", "Acceptance, context, links…": "驗收條件、背景、連結…", "What happened, what you expected, what you tried…": "發生了什麼、預期結果，以及已嘗試的方法…", "Could not create issue:": "無法建立議題：", "Draft saved.": "草稿已儲存。", "Writes are unavailable.": "目前無法寫入。", "File": "提交", "Cannot be emailed": "無法寄送電子郵件", "None": "無",
  "Customers list": "客戶清單", "Search name, email or tag…": "搜尋姓名、電子郵件或標籤…", "Import CSV": "匯入 CSV", "Sign in as the operator to inspect customer records.": "請以操作員身分登入以檢視客戶記錄。", "Read-only view. Record creation and edits are unavailable.": "唯讀檢視。無法建立或編輯記錄。", "Administrator CRM setup is required before customers open.": "請先完成管理員 CRM 設定，才能開啟客戶。", "Reading customers…": "正在讀取客戶…", "No customers match this search": "沒有符合此搜尋條件的客戶", "Create the first record with New customer, or import a CSV. Only real server rows appear here.": "使用「新增客戶」建立第一筆記錄，或匯入 CSV。此處僅顯示伺服器中的實際記錄。", "Clear the search to see every record.": "清除搜尋即可查看所有記錄。", "Filter customers on this page": "篩選此頁客戶", "Email consent: any": "電子郵件同意：不限", "Filter by email consent": "依電子郵件同意篩選", "Source: any": "來源：不限", "Filter by source": "依來源篩選", "selected": "已選取", "No customers on this page match the filters": "此頁沒有符合篩選條件的客戶", "Clear a filter to see this page's rows again.": "清除篩選條件以重新顯示此頁記錄。", "Display name": "顯示名稱", "Email (optional)": "電子郵件（選填）", "Phone (optional)": "電話（選填）", "Tags (optional)": "標籤（選填）", "Comma separated": "以逗號分隔", "Source (optional)": "來源（選填）", "Email consent (explicit)": "電子郵件同意（明確）", "SMS consent (explicit)": "簡訊同意（明確）", "No marketing until granted": "取得同意前不得寄送行銷訊息", "name@example.com": "name@example.com", "vip, newsletter": "vip, newsletter", "import": "匯入", "— duplicates and stale writes are refused by the server.": "— 伺服器會拒絕重複資料與過期寫入。", "Read-only view. A verified operator creates customer records.": "唯讀檢視。僅已驗證的操作員可建立客戶記錄。", "Administrator CRM setup is required before creating a customer.": "請先完成管理員 CRM 設定，才能建立客戶。", "Reading the record…": "正在讀取記錄…", "Consent": "同意狀態", "Profile": "個人資料", "Customer fields": "客戶欄位", "Membership is shown on each segment.": "客群成員資訊顯示於各客群頁面。", "Send history is shown on each campaign.": "寄送歷程顯示於各行銷活動頁面。", "No activity recorded yet.": "尚無活動記錄。", "Record diagnostics": "記錄診斷資訊", "Selection is chat context only — the server re-proves scope on every send.": "選取項目僅作為對話情境 — 伺服器每次傳送時都會重新驗證範圍。", "Email marketing": "電子郵件行銷", "Can receive campaigns": "可接收行銷活動", "SMS marketing": "簡訊行銷", "Not recorded": "未記錄", "Per customer choice": "依客戶選擇", "Segments list": "客群清單", "Saved rules over customer tags, source, consent and email domain.": "依客戶標籤、來源、同意狀態與電子郵件網域建立的已儲存規則。", "Sign in as the operator to inspect saved segments.": "請以操作員身分登入以檢視已儲存的客群。", "Read-only view. Segment creation and edits are unavailable.": "唯讀檢視。無法建立或編輯客群。", "Administrator CRM setup is required before segments open.": "請先完成管理員 CRM 設定，才能開啟客群。", "Reading segments…": "正在讀取客群…", "Create the first saved rule with New segment. Only real server rows appear here.": "使用「新增客群」建立第一個已儲存規則。此處僅顯示伺服器中的實際記錄。", "Segments table — scroll horizontally to reach every column": "客群表格 — 水平捲動以查看所有欄位", "Rule": "規則", "Matches": "符合數量", "Can be emailed": "可寄送電子郵件", "more": "更多", "Add rule": "新增規則", "— exact recipient counts render on the detail after Create.": "— 建立後會在詳細資料中顯示精確收件人數。", "Read-only view. A verified operator creates saved segments.": "唯讀檢視。僅已驗證的操作員可建立已儲存客群。", "Administrator CRM setup is required before creating a segment.": "請先完成管理員 CRM 設定，才能建立客群。", "Segment name": "客群名稱", "Reading host audience counts…": "正在讀取主機受眾數量…", "Base matches": "符合基本條件", "Saved exclusions": "已儲存的排除項目", "Invalid address": "無效地址", "No consent": "未取得同意", "Unsubscribed": "已取消訂閱", "Suppressed": "已抑制", "Final recipients": "最終收件人", "Preview digest": "預覽摘要", "Host audience digest": "主機受眾摘要", "Can email": "可寄送電子郵件", "Segment rule": "客群規則", "Who can be emailed": "可寄送電子郵件的對象", "Members preview": "成員預覽", "Members": "成員", "No one can be emailed from this segment yet.": "此客群目前沒有可寄送電子郵件的對象。", "View all": "檢視全部", "All members": "所有成員", "Showing": "顯示", "customers who can be emailed.": "位可寄送電子郵件的客戶。", "Current matches": "目前符合數量", "Denied": "已拒絕", "In person (counter / event)": "親自告知（櫃檯／活動）", "Website sign-up form": "網站註冊表單", "Written / email reply": "書面／電子郵件回覆", "Imported list (source noted)": "匯入名單（已註明來源）", "Other": "其他", "Choose…": "請選擇…", "Granted": "已同意", "Withdrawn": "已撤回", "Unknown": "未知", "Audience frozen": "受眾已凍結", "recipients": "位收件人", "checked just now": "剛才已檢查", "Audience changed since you froze it": "受眾自凍結後已有變更", "was": "原為", "now": "目前", "refreeze to continue.": "請重新凍結以繼續。", "Use audience · refresh snapshot": "使用受眾 · 更新快照", "Use audience · create snapshot": "使用受眾 · 建立快照", "Recheck": "重新檢查", "Valid": "有效", "Invalid": "無效", "Campaign details": "行銷活動詳細資料", "Campaign ID": "行銷活動 ID", "Reading the campaign…": "正在讀取行銷活動…", "This campaign exists only as a pending assistant draft — no content is saved yet. Review it below and Apply to save the first revision, or Discard it.": "此行銷活動目前僅為待處理的助理草稿 — 尚未儲存內容。請在下方審查並套用以儲存第一個版本，或捨棄草稿。", "Frozen audience": "已凍結受眾", "Freeze": "凍結", "A freeze snapshots exactly who receives this campaign, so later customer changes cannot alter it. The audience above is the freeze — no ids to handle: freeze it, and recheck whenever the audience changes.": "凍結會精確記錄此行銷活動的收件人，因此之後的客戶變更不會影響收件人。上方受眾即為凍結內容 — 無須處理 ID：凍結受眾，並在受眾變更時重新檢查。", "Checking the freeze…": "正在檢查凍結狀態…", "Frozen recipients": "已凍結收件人", "Current recount": "目前重新計算", "Validity": "有效性", "Technical details": "技術詳細資料", "Freeze ID (derived from the audience)": "凍結 ID（由受眾衍生）", "Frozen audience digest": "已凍結受眾摘要", "Leave with unsaved email changes?": "離開並捨棄未儲存的電子郵件變更？", "Your email draft has not been saved. Leave this campaign and discard those local edits?": "電子郵件草稿尚未儲存。離開此行銷活動並捨棄本機編輯內容？", "Leave campaign": "離開行銷活動", "Leave page": "離開頁面", "Campaign sends": "行銷活動寄送", "Sends of this campaign": "此行銷活動的寄送紀錄", "Reading sends…": "正在讀取寄送紀錄…", "No sends of this campaign yet — prepare and approve above.": "此行銷活動尚無寄送紀錄 — 請先在上方準備並核准。", "Sends": "寄送紀錄", "digest": "摘要", "Hide": "隱藏", "Show": "顯示", "prepared": "已準備", "submitting": "提交中", "failed": "失敗", "uncertain": "結果不確定", "suppressed": "已抑制", "closed": "已結束", "Attach files": "附加檔案", "Attach files unavailable": "無法附加檔案", "Message to Assistant": "傳送訊息給助理", "Campaigns list": "行銷活動清單", "Versioned email content with content-only approval. Audience freezes and test-send receipts live on each campaign's page.": "具版本的電子郵件內容與內容核准。受眾凍結與測試寄送收據位於各行銷活動頁面。", "Sign in as the operator to inspect campaigns.": "請以操作員身分登入以檢視行銷活動。", "Read-only view. Campaign creation and edits are unavailable.": "唯讀檢視。無法建立或編輯行銷活動。", "Administrator CRM setup is required before campaigns open.": "請先完成管理員 CRM 設定，才能開啟行銷活動。", "Reading campaigns…": "正在讀取行銷活動…", "Create the first campaign with New campaign — audience, editor, preview and test-send all live there, never on this list. Only real server rows appear here.": "使用「新增行銷活動」建立第一個活動 — 受眾、編輯器、預覽與測試寄送都在活動頁面，不會出現在此清單。此處僅顯示伺服器中的實際記錄。", "Campaigns table — scroll horizontally to reach every column": "行銷活動表格 — 水平捲動以查看所有欄位", "Campaign": "行銷活動", "Content": "內容", "Latest send": "最近寄送", "The saved content is approved": "已核准已儲存的內容", "The saved content is approved on an earlier saved version": "已核准較早版本的已儲存內容", "The saved content is not approved yet": "尚未核准已儲存的內容", "Approved": "已核准", "Needs review — edited since approval": "需要審查 — 核准後已有修改", "Draft": "草稿", "Prepared but not yet approved — nothing was sent": "已準備但尚未核准 — 尚未寄送任何內容", "Pending send": "等待寄送", "SMTP acceptance only — not proof of inbox delivery": "僅代表 SMTP 接受 — 不代表已送達收件匣", "accepted": "已接受", "Assistant drafts awaiting review": "等待審查的助理草稿", "The assistant drafted these emails from your chat. Nothing is saved until you open one and Apply.": "助理根據你的對話草擬了這些電子郵件。開啟並套用之前不會儲存任何內容。", "Untitled draft": "未命名草稿", "Review draft": "審查草稿", "messages": "訊息", "Preview closed": "預覽已關閉", "Preview updated above": "上方預覽已更新", "Working": "處理中", "Finished with an issue": "完成但有問題", "step": "個步驟", "steps": "個步驟", "New": "新增", "conversation (unsaved)": "對話（未儲存）", "New conversation (unsaved)": "新增對話（未儲存）", "The conversations could not be read": "無法讀取對話", "No conversation yet. Start one with + New.": "目前沒有對話。按「＋新增」開始對話。", "Assistant is finishing another task — your message is queued": "助理正在完成其他任務 — 你的訊息已排入佇列", "Queued…": "已排入佇列…", "Sending…": "傳送中…", "Waiting…": "等待中…", "Stop": "停止", "The thread could not be read": "無法讀取對話串", "Reading the thread…": "正在讀取對話串…", "Nothing here yet. Send the first message.": "目前沒有內容。傳送第一則訊息。", "Load earlier messages ↑": "載入較早的訊息 ↑", "New messages": "新訊息", "An unsent draft is kept — this conversation already had its own.": "未傳送的草稿已保留 — 此對話已有自己的草稿。", "unsent drafts are kept — this conversation already had its own.": "則未傳送草稿已保留 — 此對話已有自己的草稿。", "Restore saved draft": "還原已儲存草稿", "Uploading…": "上傳中…", "Read-only · Sending is unavailable": "唯讀 · 無法傳送", "Attach files (txt, md, csv — up to 10 MiB each; PDF/image processing is not available yet)": "附加檔案（txt、md、csv，每個最多 10 MiB；目前不支援 PDF／圖片處理）", "Retry or discard the previous message first": "請先重試或捨棄上一則訊息", "remove the": "移除此", "reference": "參照", "unavailable": "無法使用", "read": "讀取", "set": "設定", "action": "操作", "Conversation messages": "對話訊息", "Project issue list": "專案議題清單", "source:": "來源：", "issue folders": "議題資料夾", "chains": "記錄鏈", "cadence daemon socket": "Cadence daemon 通訊端", "writes are disabled": "寫入功能已停用", "writes commit as": "寫入將以此身分提交：", "Epic": "史詩議題", "unknown epic": "未知史詩議題", "retry": "重試", "Select all customers on this page": "選取此頁所有客戶", "Filter by tag": "依標籤篩選", "Email consent": "電子郵件同意", "SMS consent": "簡訊同意", "Email marketing consent": "電子郵件行銷同意", "SMS marketing consent": "簡訊行銷同意", "e.g. Signed at the counter, 2 Oct": "例如：10 月 2 日於櫃檯簽署", "No": "否", "Filters": "篩選條件", "hide filters": "隱藏篩選條件", "filters": "篩選條件", "tag": "標籤", "epic": "史詩議題", "component": "元件", "finished issues are hidden by default — a search still matches them": "已完成議題預設隱藏 — 搜尋仍會包含這些議題", "done": "已完成", "one swimlane per epic, with its progress": "每個史詩議題一個泳道，並顯示進度", "group by epic": "依史詩議題分組", "clear": "清除", "checklist": "檢查清單", "status derived from": "狀態衍生自", "not set by hand": "而非手動設定", "job": "工作", "derived": "衍生", "in": "位於", "waits on": "等待", "after": "之後",
  "No saved email yet. Ask the assistant in the left chat to draft this campaign's email, then use a verified proposal in the editor and explicitly Save revision 1.": "尚未儲存電子郵件。請在左側對話中要求助理草擬此行銷活動的電子郵件，接著在編輯器中使用已驗證的提案，並明確儲存第 1 個版本。",
  "Email envelope": "電子郵件信封", "Subject": "主旨", "Preheader": "前置摘要", "Optional inbox preview text": "選填的收件匣預覽文字", "Hide preheader": "隱藏前置摘要", "Add preheader": "新增前置摘要", "From": "寄件者", "To": "收件者", "each recipient": "每位收件者", "Sender material is host-locked preview-only bytes": "寄件者資料由主機鎖定，僅供預覽使用", "host footer — the unsubscribe link and sender address are added by the host and cannot be edited.": "主機頁尾 — 取消訂閱連結與寄件者地址由主機新增，無法編輯。",
  "Save keeps a sanitized body fragment, not a full document.": "儲存時會保留經清理的內文片段，而非完整文件。", "Save keeps a sanitized body fragment, not this full document.": "儲存時會保留經清理的內文片段，而非此完整文件。", "General": "一般", "The host removes the doctype and document wrappers, including head content such as the title and stylesheet blocks. Only selected supported inline style attributes may persist; unsupported or unsafe content is stripped. The protected sender and unsubscribe footer are appended separately by the host. Switching modes preserves your unsaved source, but Save does not save a full document or stylesheet verbatim.": "主機會移除 doctype 和文件包裝標籤，以及標題和樣式表區塊等 head 內容。僅特定支援的行內樣式屬性會保留；不支援或不安全的內容將被移除。受保護的寄件者與取消訂閱頁尾由主機另外附加。切換模式會保留未儲存的原始碼，但儲存時不會逐字保留完整文件或樣式表。", "HTML source": "HTML 原始碼", "Paste or edit the email body HTML": "貼上或編輯電子郵件內文 HTML", "Live HTML draft preview": "即時 HTML 草稿預覽", "Host render of the saved version": "主機呈現的已儲存版本", "Rendering the draft preview…": "正在呈現草稿預覽…", "Rendering the saved email…": "正在呈現已儲存的電子郵件…", "Visual preview of HTML draft": "HTML 草稿的視覺預覽", "Draft email preview": "電子郵件草稿預覽", "Visual email preview, saved revision": "電子郵件視覺預覽，已儲存版本", "Advanced: plain-text version": "進階：純文字版本", "Write my own": "自行撰寫", "Custom plain-text override": "自訂純文字覆寫內容", "Unsaved custom text is saved only when you choose Save.": "只有選擇儲存時，未儲存的自訂文字才會儲存。", "The host generates plain text from the email body unless you write an override.": "除非自行撰寫覆寫內容，否則主機會根據電子郵件內文產生純文字。", "Suggested plain-text render (not saved)": "建議的純文字呈現（未儲存）", "Saved custom plain-text render": "已儲存的自訂純文字呈現", "Saved host-generated plain text": "已儲存的主機產生純文字", "Host-rendered plain text": "主機呈現的純文字", "The required sender and unsubscribe footer is appended by the host and cannot be edited here.": "必要的寄件者與取消訂閱頁尾由主機附加，無法在此編輯。", "Draft body blocks": "草稿內文區塊", "Assistant proposals": "助理提案", "Replace unsaved email changes?": "要取代未儲存的電子郵件變更嗎？", "Using this proposal replaces your unsaved subject, preheader, body, and text override. Saved content stays unchanged until you choose Save.": "使用此提案會取代未儲存的主旨、前置摘要、內文及文字覆寫內容。在選擇儲存之前，已儲存的內容不會變更。", "Replace local draft": "取代本機草稿", "No pending assistant draft.": "沒有待處理的助理草稿。", "Ask the assistant in the left chat to draft or improve this email — its proposal appears here for review.": "請在左側對話中要求助理草擬或改善此電子郵件 — 提案會顯示於此供審查。", "Refresh drafts": "重新整理草稿", "Use in editor changes only the local draft. Save creates a new revision; Discard changes nothing.": "在編輯器中使用只會變更本機草稿。儲存會建立新版本；捨棄則不會變更任何內容。", "Email preview": "電子郵件預覽", "Email editing mode": "電子郵件編輯模式", "Preview width": "預覽寬度", "Desktop": "桌面", "Mobile": "行動裝置", "Sender preview (not send authorization)": "寄件者預覽（不代表授權寄送）", "Preview placeholder — no sender selected": "預覽佔位項 — 尚未選取寄件者", "Sample recipient first name (optional)": "收件者名字範例（選填）", "Refresh preview": "重新整理預覽", "Re-render the last saved version with the current sample name": "使用目前的範例名稱重新呈現最後儲存的版本", "Review this suggestion and use it in the editor; nothing is saved until you choose Save.": "請審查此建議並在編輯器中使用；選擇儲存之前不會儲存任何內容。", "Preview shows the last saved version. Save changes to refresh.": "預覽顯示最後儲存的版本。儲存變更以重新整理。", "Read-only view. A verified operator saves content revisions.": "唯讀檢視。僅已驗證的操作員可儲存內容版本。", "Unsaved changes to v": "未儲存的變更：版本", "Saving creates v": "儲存將建立版本", "and resets approval": "並重設核准狀態", "A newer version was saved while you were editing — your text is kept, but saving is pinned to v": "編輯期間已有較新版本儲存 — 你的文字會保留，但儲存會固定於版本", "and will be refused rather than overwrite it.": "且將被拒絕，而不會覆寫較新版本。", "Reloading latest…": "正在重新載入最新版本…", "Discard": "捨棄", "Save as v": "另存為版本",
  "Email canvas — edit directly": "電子郵件畫布 — 直接編輯", "Segment members": "客群成員", "No saved email yet. Ask the assistant in the left chat to draft this campaign's email, then Apply its verified proposal below to create revision 1.": "尚未儲存電子郵件。請在左側對話中要求助理草擬此行銷活動的電子郵件，接著套用下方已驗證的提案以建立第 1 個版本。", "Sample first name (optional)": "名字範例（選填）", "Ada": "Ada", "Saved render": "已儲存的呈現", "Preview format": "預覽格式", "Visual": "視覺", "saved version": "已儲存版本", "Email content": "電子郵件內容", "not drafted yet": "尚未草擬", "revision": "版本", "No email draft yet. Ask the assistant in the left chat to draft this campaign's email, then Apply its verified proposal below to create revision 1 — or use the inline editor after a draft exists.": "尚無電子郵件草稿。請在左側對話中要求助理草擬此行銷活動的電子郵件，接著套用下方已驗證的提案以建立第 1 個版本；或在草稿建立後使用行內編輯器。", "Body": "內文", "HTML body": "HTML 內文", "Edit the email on the campaign's Email tab.": "請在行銷活動的「電子郵件」分頁編輯郵件。", "Content approval": "內容核准", "Approval — content-only": "核准 — 僅限內容", "Content-only approval on this revision": "此版本僅核准內容", "No content approval on this revision. Any content edit invalidates approval.": "此版本尚未核准內容。任何內容編輯都會使核准失效。", "Approval never sends — the bounded send below is a separate operator decision over exact revisions.": "核准不會寄送 — 下方受限寄送是操作員針對確切版本所做的另一項決策。", "Test send": "測試寄送", "Test send — one operator address": "測試寄送 — 一個操作員地址", "Sends the exact frozen content through the campaign sender to one operator-typed address. The receipt records acceptance or refusal only — a campaign send needs one accepted test send of this exact content and sender.": "透過行銷活動寄件者，將完全相同的凍結內容寄送至一個由操作員輸入的地址。收據僅記錄接受或拒絕 — 行銷活動寄送須先以完全相同的內容與寄件者成功測試寄送一次。", "No sender is set up": "尚未設定寄件者", "set up sending": "設定寄送", "to send a test.": "以進行測試寄送。", "Test recipient (one operator address)": "測試收件者（一個操作員地址）", "Send one test message through the campaign sender": "透過行銷活動寄件者傳送一封測試郵件", "Send test": "傳送測試郵件", "Test-send receipt": "測試寄送收據", "Result": "結果", "Recipient": "收件者", "Claim": "聲明", "Nothing was sent yet. Send the same test again once the owner has approved it.": "尚未寄送任何郵件。負責人核准後，請再次傳送相同測試郵件。", "The sender accepted the message — this is not proof of inbox delivery.": "寄件者已接受郵件 — 這不代表郵件已送達收件匣。", "Proposals — Apply or Discard": "提案 — 套用或捨棄", "Only Apply changes the saved version (approval invalidates); Discard is non-mutating. Nothing proposes, edits or sends silently.": "只有套用會變更已儲存版本（並使核准失效）；捨棄不會變更內容。任何提案、編輯或寄送都不會在未提示下執行。", "Ask the assistant in the left chat to draft this email — its proposal appears below for review.": "請在左側對話中要求助理草擬此電子郵件 — 提案會顯示於下方供審查。", "No pending proposals for this campaign.": "此行銷活動沒有待處理的提案。", "Pending proposals": "待處理提案", "Read-only conversation": "唯讀對話", "slash commands": "斜線命令", "cited rows": "引用的資料列", "Expand assistant chat": "展開助理對話", "New reply waiting": "有新回覆等待查看", "ASSISTANT": "助理", "Conversation history": "對話歷程", "Collapse assistant chat": "收合助理對話", "Link to this conversation": "此對話的連結", "Shareable link to this conversation": "此對話的可分享連結", "A shareable link needs a verified context": "可分享連結需要已驗證的情境", "Working…": "處理中…", "Stop the current turn (applies to the whole assistant, not only this conversation)": "停止目前回合（會影響整個助理，而非僅此對話）", "Stops the assistant's running turn. The board command is global, not scoped to this conversation, and cancels the turn only — it cannot undo a committed effect.": "停止助理目前執行中的回合。看板命令是全域命令，不限於此對話，且只會取消回合 — 無法復原已提交的效果。", "Not sent — the conversation could not be created.": "尚未傳送 — 無法建立對話。", "Message Assistant…": "傳送訊息給助理…",
 "Email heading": "電子郵件標題", "block": "區塊", "Heading": "標題", "Email paragraph": "電子郵件段落", "Write your message": "撰寫訊息", "Button label": "按鈕標籤", "Button link": "按鈕連結", "Block": "區塊", "actions": "動作", "Move block": "移動區塊", "up": "上移", "down": "下移", "Remove block": "移除區塊", "Start with text, a heading, or your own HTML.": "從文字、標題或自己的 HTML 開始。", "Undo last block change": "復原上次區塊變更", "Click to edit · ⋯ for block actions · The unsubscribe footer is always included.": "點選即可編輯 · ⋯ 可執行區塊動作 · 一律會包含取消訂閱頁尾。", "Add block": "新增區塊", "Text": "文字", "Button": "按鈕", "Nothing saved": "尚未儲存", "saved": "已儲存", "Host-verified: the assistant produced this draft on this campaign — the browser's copy is never the authority": "主機已驗證：助理為此行銷活動產生此草稿 — 瀏覽器副本絕非可信依據", "Verified assistant draft": "已驗證的助理草稿", "Submitted through the operator proposal route — no assistant provenance": "透過操作員提案路徑提交 — 無助理來源證明", "Operator-submitted": "操作員提交", "from": "來自", "Preview version": "預覽版本", "This suggestion was based on an older saved revision; review it again before use": "此建議是根據較舊的已儲存版本產生；使用前請重新審查", "Copy this host-attributed suggestion into the unsaved editor draft": "將此主機歸屬建議複製到未儲存的編輯器草稿", "Review the suggestion before using it in the editor": "在編輯器中使用前請先審查此建議", "Use in editor": "在編輯器中使用", "actor": "操作者", "origin": "來源", "source": "依據", "Using this suggestion changes the unsaved draft body format; it does not save until you choose Save.": "使用此建議會變更未儲存草稿的內文格式；選擇儲存之前不會儲存內容。", "Needs review (stale) — drafted against r": "需要審查（已過期）— 草稿依據版本", ", the saved version is now r": "，目前已儲存版本為 r", "Re-review its text before asking the assistant to draft again.": "請重新審查文字，再要求助理重新草擬。",
  "Customers where": "符合以下條件的客戶", "and": "且", "Match the rule": "符合規則", "Valid email": "有效電子郵件", "have not agreed to receive email": "尚未同意接收電子郵件", "have unsubscribed": "已取消訂閱", "have no valid email address": "沒有有效的電子郵件地址", "are on the suppression list": "位於抑制清單中", "are on the saved exclusion list": "位於已儲存的排除清單中", "Nobody can be emailed:": "無法寄送電子郵件給任何人：", "customer": "位客戶", "customers": "位客戶", "No customers match this rule yet.": "目前沒有客戶符合此規則。", "Segment rules": "客群規則", "Field": "欄位", "field": "欄位", "Operator": "運算子", "operator": "運算子", "Value": "值", "Remove rule": "移除規則", "Tag": "標籤", "Source": "來源", "Email domain": "電子郵件網域", "is": "是", "is not": "不是", "customers carrying this tag (letters, digits, - _)": "具有此標籤的客戶（英文字母、數字、-、_）", "the import source label (letters, digits, - _)": "匯入來源標籤（英文字母、數字、-、_）", "granted, denied or unknown": "granted、denied 或 unknown", "the part after @, lowercased (example.com)": "@ 後方的小寫部分（example.com）",
};

Object.assign(zhTW, {
  "Delivery checkpoints": "交付檢查點", "View roadmap": "查看路線圖", "Reading milestones…": "正在讀取里程碑…", "Milestones could not be loaded": "無法載入里程碑", "Planned": "已規劃", "Active": "進行中", "Achieved": "已達成", "Cancelled": "已取消", "Status unknown": "狀態未知", "Status not set": "尚未設定狀態", "Needs definition": "需要定義", "Target": "目標日期", "Not scheduled": "尚未排程", "{days} days overdue": "逾期 {days} 天", "Due today": "今天到期", "{count} checkpoints are referenced by work and need definitions.": "有 {count} 個檢查點被工作引用，仍需定義。", "No active or planned checkpoints.": "沒有進行中或已規劃的檢查點。",
  "Campaign sections": "行銷活動區段", "Overview": "總覽", "Email": "電子郵件", "Audience": "受眾", "Activity": "活動歷程", "1 draft": "1 份草稿",
  "Next task": "下一個任務", "All test and review prerequisites are recorded.": "測試與審查的所有先決條件均已記錄。", "Final send still requires a separate guarded operator review.": "最終寄送仍須經過獨立且受保護的操作員審查。", "Review final send": "審查最終寄送", "Requires": "需要", "Email drafted": "電子郵件已草擬", "Content approved": "內容已核准", "Audience frozen and still valid": "受眾已凍結且仍有效", "Email sender connected": "已連結電子郵件寄件者", "Test email accepted": "測試郵件已接受", "Review": "審查", "Approve": "核准", "Fix": "修正", "Set up sending": "設定寄送", "Send a test": "傳送測試郵件", "a frozen audience": "已凍結的受眾", "the audience frozen and rechecked (its validity is unverified)": "已凍結並重新檢查的受眾（有效性尚未確認）", "the audience frozen and reporting valid": "已凍結且回報有效的受眾", "the sender binding read (still loading)": "寄件者連結讀取完成（仍在載入）", "a live SMTP sender binding": "有效的 SMTP 寄件者連結", "an accepted test send of this content and binding": "此內容與寄件者連結的測試郵件已接受", "a test send accepted against this exact content revision and binding": "針對此確切內容版本與寄件者連結已接受的測試郵件", "Campaign summary": "行銷活動摘要", "Checking…": "檢查中…", "No sender connected · Set up": "未連結寄件者 · 前往設定", "via AgenticOS": "透過 AgenticOS", "Not drafted": "尚未草擬", "No saved revision": "沒有已儲存的版本", "content approved only": "僅核准內容", "needs content review": "需要審查內容",
  "Delivery rows — scroll horizontally to reach every column": "寄送資料列 — 水平捲動以查看所有欄位", "State": "狀態", "Attempts": "嘗試次數", "Reason": "原因", "Resolve": "處理", "by": "由", "Mark this delivery": "標記此寄送為", "Mark": "標記", "Reading the send…": "正在讀取寄送狀態…", "Send progress": "寄送進度", "Delivery counts": "寄送數量", "submitted": "已提交", "Prepare commits content revision + digest, the audience freeze, the sender link and the unsubscribe origin into one send digest the operator then approves. Any material change between prepare and approve refuses the send. SMTP acceptance is recorded — never inbox delivery, never reads.": "準備時會將內容版本與摘要、受眾凍結、寄件者連結及取消訂閱來源整合為一份寄送摘要，再由操作員核准。準備與核准之間若任何資料變更，寄送將遭拒。系統記錄 SMTP 接受狀態 — 絕不代表送達收件匣或已讀。", "Final send — approved, bounded, no resend": "最終寄送 — 須核准、有界限、不可重複寄送", "Final send": "最終寄送", "Prepare stays unavailable — missing:": "目前無法準備寄送 — 尚缺：", "Every prerequisite is in place — approved revision": "所有先決條件均已具備 — 已核准版本", "the frozen audience (checked just now), sender": "已凍結的受眾（剛才已檢查），寄件者", "accepted test send of this content.": "此內容的測試寄送已接受。", "Prepare again.": "請重新準備。", "Freeze the send's material and show what approval commits": "凍結寄送資料並檢視核准內容", "Missing:": "尚缺：", "Prepare send": "準備寄送", "Prepared send": "已準備的寄送", "Send": "寄送", "state": "狀態", "Included": "包含", "Excluded": "排除", "Suppressed now": "目前抑制", "Final recipients": "最終收件者", "ceiling": "上限", "Audience freeze": "受眾凍結", "Unsubscribe origin": "取消訂閱來源", "Send digest": "寄送摘要", "Sample (masked):": "範例（已遮蔽）：", "Approve and send…": "核准並寄送…", "Discard prepared view": "捨棄準備資料", "Approve send of": "核准寄送給", "exactly as prepared —": "完全依照準備內容 —", "recipients, content revision": "位收件者，內容版本", "The host re-verifies every input; a refusal discards this prepared view.": "主機會重新驗證所有輸入；若遭拒，將捨棄此準備資料。", "Type": "輸入", "the final recipient count": "最終收件者數量", "to approve": "以核准", "Approve and send": "核准並寄送", "This audience is not frozen yet. Freezing snapshots exactly who receives the campaign, so later customer changes cannot alter it.": "此受眾尚未凍結。凍結會精確記錄行銷活動的收件者，因此之後的客戶變更不會影響收件者。", "The frozen digest still matches the live audience": "凍結摘要仍與目前受眾相符", "Freeze validity": "凍結有效性",
});

Object.assign(zhTW, {
  "Send to": "寄送對象", "Audience — one base mode": "受眾 — 選擇一種基礎模式", "Base audience mode": "受眾基礎模式", "All eligible customers": "所有符合資格的客戶", "One saved segment": "一個已儲存的客群", "Custom customer IDs": "自訂客戶 ID", "none saved yet": "尚未儲存", "Saved segment": "已儲存的客群", "Customer IDs (comma or space separated — the final suppression union still applies)": "客戶 ID（以逗號或空格分隔 — 最終仍會套用所有抑制項目）", "Advanced: exclusions and suppressions": "進階：排除名單與抑制項目", "Audience preview": "受眾預覽", "This audience is not frozen yet. Freezing snapshots exactly who receives the campaign, so later customer changes cannot alter it.": "此受眾尚未凍結。凍結會精確記錄行銷活動的收件者，因此之後的客戶變更不會影響收件者。", "The frozen digest still matches the live audience": "凍結摘要仍與目前受眾相符", "Freeze validity": "凍結有效性", "Digest": "摘要", "draft": "份草稿",
  "Accepted": "已接受", "Refused": "已拒絕", "Mark accepted": "標記為已接受", "Mark failed": "標記為失敗", "The daemon lost the submission's answer, so this row is uncertain — the message may already have been sent. Marking it": "Daemon 未收到提交結果，因此此資料列的狀態不確定 — 郵件可能已寄出。將其標記為", "records your reconciliation only:": "僅記錄你的核對結果：", "no resend happens either way": "無論選擇何者都不會再次寄送", "Failed": "失敗", "prepared": "已準備", "queued": "排隊中", "submitting": "提交中", "failed": "失敗", "uncertain": "結果不確定", "suppressed": "已抑制", "closed": "已結束", "smtp": "SMTP", "SMTP accepted the message — this is not proof of inbox delivery.": "SMTP 已接受郵件 — 這不代表郵件已送達收件匣。",
  "Everything is ready": "一切就緒", "Requires": "需要", "Checking…": "檢查中…", "Verified assistant draft": "已驗證的助理草稿", "Host-verified: the assistant produced this draft on this campaign — the browser's copy is never the authority": "主機已驗證：助理為此行銷活動產生此草稿 — 瀏覽器中的副本並非可信依據", "Operator-submitted": "操作員提交", "Submitted through the operator proposal route — no assistant provenance": "透過操作員提案路徑提交 — 無助理來源證明", "Hide preview": "隱藏預覽", "Preview draft": "預覽草稿", "Rendering the draft preview…": "正在呈現草稿預覽…", "Preview-only sender material — host-locked": "僅供預覽的寄件者資料 — 由主機鎖定", "preview-only": "僅供預覽", "Draft preview format": "草稿預覽格式", "Draft email preview": "電子郵件草稿預覽", "Subject:": "主旨：", "Preheader:": "前置摘要：", "Draft body blocks": "草稿內文區塊", "Button:": "按鈕：", "Needs review (stale) — drafted against revision": "需要審查（已過期）— 草稿依據版本", "the saved version is now revision": "目前已儲存版本為版本", "Re-review its text before asking the assistant to draft again.": "請重新審查文字，再要求助理重新草擬。", "Apply is disabled: the proposal's stamped source revision is behind the current draft": "無法套用：提案標記的來源版本早於目前草稿", "Apply as a new revision (expects the draft at revision": "套用為新版本（預期草稿版本為", "Apply (new revision)": "套用（新版本）", "Sender in this render — preview": "此呈現中的寄件者 — 預覽", "From": "寄件者", "Unsubscribe": "取消訂閱", "Binding": "連結", "unexpected — reported": "異常 — 已回報", "The render names the host's preview sender; the real sender is the bound SMTP connection above, and every send's unsubscribe links build on the configured origin. A preview proves the bytes only — sending is the operator's separate decision below.": "此呈現使用主機的預覽寄件者；實際寄件者是上方連結的 SMTP 連線，且每封郵件的取消訂閱連結皆依據已設定的來源建立。預覽僅能證明內容位元組 — 寄送仍由操作員在下方另行決定。", "Content approval — separate decision": "內容核准 — 獨立決策", "Test email — separate action": "測試郵件 — 獨立操作", "Final send — separate operator review": "最終寄送 — 操作員獨立審查", "Retry": "重試", "Reading the campaign…": "正在讀取行銷活動…", "No sends of this campaign yet — prepare and approve above.": "此行銷活動尚無寄送紀錄 — 請先在上方準備並核准。", "The campaign request was refused — retry.": "行銷活動請求遭拒 — 請重試。", "The audience request was refused — retry.": "受眾請求遭拒 — 請重試。", "The send request was refused — retry.": "寄送請求遭拒 — 請重試。",  "All customers": "所有客戶", "Segment": "客群", "chosen customers": "位選取的客戶", "not counted yet": "尚未計算", "match": "位符合條件", "can be emailed": "位可寄送電子郵件", "content approved at the current revision": "目前版本的內容已核准", "No sender connected · Set up": "未連結寄件者 · 前往設定",  "the sender binding read (still loading)": "寄件者連結讀取完成（仍在載入）", "the audience frozen and rechecked (its validity is unverified)": "已凍結並重新檢查的受眾（有效性尚未確認）", "the audience frozen and reporting valid": "已凍結且回報有效的受眾", "Approved r": "已核准版本", "No content approval on this revision. Any content edit invalidates approval.": "此版本尚未核准內容。任何內容編輯都會使核准失效。",
});

Object.assign(zhTW, {
  "Waiting for owner approval in AgenticOS": "等待 AgenticOS 擁有者核准", "a test recipient address is required": "必須提供測試收件者地址", "Approve revision": "核准版本", "Approve r": "核准版本 r", "content-only": "僅限內容", "Applied as revision": "已套用為版本", "content approval invalidated; re-approve before any send preparation.": "內容核准已失效；準備寄送前請重新核准。", "Proposal": "提案", "discarded — still no saved revision.": "已捨棄 — 尚無已儲存版本。", "discarded — draft unchanged at revision": "已捨棄 — 草稿仍維持版本", "discarded — draft unchanged at r": "已捨棄 — 草稿仍維持版本", "This audience is not frozen yet. Freezing snapshots exactly who receives the campaign, so later customer changes cannot alter it.": "此受眾尚未凍結。凍結會精確記錄行銷活動的收件者，因此之後的客戶變更不會影響收件者。", "Campaign send": "行銷活動寄送", "retry": "重試",
});

Object.assign(zhTW, {
  "open issues": "個未結議題", "done shown": "個已完成議題已顯示", "done hidden": "個已完成議題已隱藏", "dropped issues": "個已放棄議題", "active issues": "項進行中", "blocked issues": "項受阻",
  "Assistant draft": "✦ 助理草稿", "Describe it; the assistant writes it": "描述內容，由助理代為撰寫", "Create and draft": "建立並草擬", "Paste HTML": "貼上 HTML", "Bring an existing email": "匯入現有電子郵件", "Create from HTML": "從 HTML 建立", "Blank": "空白", "Write it with blocks": "使用區塊撰寫", "Create blank campaign": "建立空白行銷活動", "Send brief again": "重新傳送需求",
  "Give the campaign a short plain name (up to 80 characters).": "請為行銷活動輸入簡短純文字名稱（最多 80 個字元）。", "The audience request was refused — retry.": "受眾請求遭拒 — 請重試。", "Your session expired or the connection dropped — reload the page to sign in again.": "工作階段已逾期或連線中斷 — 請重新載入頁面以再次登入。",  "Give the campaign a name.": "請輸入行銷活動名稱。", "Describe what the email should say so the assistant can draft it.": "請描述電子郵件內容，讓助理為你草擬。", "Paste the email HTML to start from it.": "請貼上電子郵件 HTML 以開始。", "The campaign is saved, but the brief was not sent to the assistant:": "行銷活動已儲存，但未能將需求傳送給助理：", "request failed": "要求失敗", "New campaign": "新增行銷活動", "Name it and choose how to start. You can change everything later.": "為行銷活動命名並選擇開始方式。之後仍可變更所有內容。", "Name": "名稱", "Start with": "開始方式", "What should it say?": "電子郵件內容", "Welcome new customers warmly, keep it short, and link to the booking page.": "親切地歡迎新客戶，內容簡短，並附上預約頁面連結。", "HTML": "HTML", "Scripts, forms and tracking pixels are removed; the unsubscribe footer is added for you": "指令碼、表單與追蹤像素會移除，並自動加入取消訂閱頁尾。", "<h1>Hello</h1>…": "<h1>您好</h1>…", "Opens the editor with a heading and a paragraph.": "開啟含有標題與段落的編輯器。", "Audience": "受眾", "optional — you can choose later": "選填 — 之後也可以選擇", "Decide later": "稍後決定", "Nothing is sent from here.": "此處不會寄送任何內容。", "Open without the brief": "不傳送需求直接開啟", "Cancel": "取消",
});

function validLocale(value: unknown): value is Locale {
  return value === "en" || value === "zh-TW";
}

function browserLocale(): Locale {
  if (typeof navigator === "undefined") return "en";
  const languages = navigator.languages?.length ? navigator.languages : [navigator.language];
  for (const language of languages) {
    if (/^zh-(TW|Hant)(-|$)/i.test(language)) return "zh-TW";
    if (/^en(-|$)/i.test(language)) return "en";
  }
  return "en";
}

async function readPreferences(): Promise<Preferences> {
  const response = await fetch("/api/locale", { headers: { "X-Cadence-Board": "1" } });
  if (!response.ok) throw new Error(`Locale preferences unavailable (${response.status})`);
  const value = await response.json() as Partial<Preferences>;
  return {
    organization_locale: validLocale(value.organization_locale) ? value.organization_locale : null,
    user_locale: validLocale(value.user_locale) ? value.user_locale : null,
  };
}

async function writePreference(scope: Scope, locale: Locale | null): Promise<Preferences> {
  const response = await fetch("/api/locale", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Cadence-Board": "1" },
    body: JSON.stringify({ scope, locale }),
  });
  if (!response.ok) throw new Error(`Locale preference write refused (${response.status})`);
  const value = await response.json() as Partial<Preferences>;
  return {
    organization_locale: validLocale(value.organization_locale) ? value.organization_locale : null,
    user_locale: validLocale(value.user_locale) ? value.user_locale : null,
  };
}

interface LocaleContextValue {
  locale: Locale;
  userLocale: Locale | null;
  organizationLocale: Locale | null;
  isHostedUser: boolean;
  canSetOrganizationLocale: boolean;
  t: (english: string) => string;
  setSession: (meta: Meta | null) => void;
  setPreference: (scope: Scope, locale: Locale | null) => Promise<void>;
  formatDate: (value: string | number | Date, options?: Intl.DateTimeFormatOptions) => string;
  formatNumber: (value: number, options?: Intl.NumberFormatOptions) => string;
  formatRelativeTime: (value: string | number | Date, now?: number) => string;
}

const LocaleContext = createContext<LocaleContextValue | null>(null);

export function LocaleProvider({ children }: { children: ReactNode }) {
  const [meta, setMeta] = useState<Meta | null>(null);
  const [storedPreferences, setStoredPreferences] = useState<{ identity: string; value: Preferences } | null>(null);
  const requestId = useRef(0);
  const identity = meta?.hosted === true && meta.signed_in === true && meta.session?.origin === "public" && meta.session.user?.sub
    ? `${meta.session.user.sub}:${meta.session.id}`
    : null;

  const setSession = useCallback((next: Meta | null) => setMeta(next), []);

  useEffect(() => {
    const request = ++requestId.current;
    if (!identity) {
      return;
    }
    readPreferences()
      .then((next) => { if (request === requestId.current) setStoredPreferences({ identity, value: next }); })
      .catch(() => { if (request === requestId.current) setStoredPreferences(null); });
    return () => { requestId.current += 1; };
  }, [identity]);

  const preferences = storedPreferences?.identity === identity ? storedPreferences.value : null;
  const locale: Locale = identity
    ? preferences?.user_locale ?? preferences?.organization_locale ?? browserLocale()
    : browserLocale();
  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  const setPreference = useCallback(async (scope: Scope, value: Locale | null) => {
    requestId.current += 1;
    const next = await writePreference(scope, value);
    if (identity) setStoredPreferences({ identity, value: next });
  }, [identity]);

  const context = useMemo<LocaleContextValue>(() => ({
    locale,
    userLocale: preferences?.user_locale ?? null,
    organizationLocale: preferences?.organization_locale ?? null,
    isHostedUser: identity !== null,
    canSetOrganizationLocale: identity !== null && meta?.session?.user?.role === "operator",
    t: (english) => locale === "zh-TW" ? (zhTW[english] ?? english) : english,
    setSession,
    setPreference,
    formatDate: (value, options) => new Intl.DateTimeFormat(locale, options).format(value instanceof Date ? value : new Date(value)),
    formatNumber: (value, options) => new Intl.NumberFormat(locale, options).format(value),
    formatRelativeTime: (value, now = Date.now()) => {
      const seconds = (new Date(value instanceof Date ? value.getTime() : value).getTime() - now) / 1000;
      const units: [Intl.RelativeTimeFormatUnit, number][] = [["year", 31_536_000], ["month", 2_592_000], ["week", 604_800], ["day", 86_400], ["hour", 3_600], ["minute", 60], ["second", 1]];
      const [unit, size] = units.find(([, unitSeconds]) => Math.abs(seconds) >= unitSeconds) ?? ["second", 1];
      return new Intl.RelativeTimeFormat(locale, { numeric: "auto" }).format(Math.round(seconds / size), unit);
    },
  }), [identity, locale, meta?.session?.user?.role, preferences, setPreference, setSession]);

  return <LocaleContext.Provider value={context}>{children}</LocaleContext.Provider>;
}

Object.assign(zhTW, {
  "Breadcrumb": "導覽階層",
  "Open navigation": "開啟導覽選單",
  "Issue could not be loaded.": "無法載入議題。",
  "Lane could not be loaded.": "無法載入工作區。",
  "History could not be loaded.": "無法載入歷程。",
  "Lane": "工作區",
  "Refreshing": "正在重新整理",
  "Showing the last known information.": "顯示上次已知的資訊。",
  "Workspace apps are hidden because this browser isn't signed in as a team member. Sign in to see them.": "此瀏覽器尚未以團隊成員身分登入，因此隱藏工作區應用程式。請登入以檢視。",
  "Pin": "釘選",
  "Unpin": "取消釘選",
  "Manage": "管理",
  "Update": "更新",
  "Sort": "排序",
  "This app changed after you checked it. Nothing was installed. Check it again to review the current version.": "檢查後此應用程式已有變更。未安裝任何內容。請重新檢查以檢視目前版本。",
  "You're not signed in as an admin. Sign in to see and manage your apps.": "你尚未以管理員身分登入。請登入以檢視及管理應用程式。",
  "The 30-day restore window has closed, so this app can only stay removed.": "30 天還原期限已過，此應用程式只能維持移除狀態。",
  "This app is already in your workspace. If you removed it, restore it from Apps.": "此應用程式已在你的工作區中。若已移除，請從「應用程式」中還原。",
  "This app is removed. Restore it first.": "此應用程式已移除。請先還原。",
  "Sign in to view your apps": "登入以檢視你的應用程式",
  "Sign in to view workspace apps": "登入以檢視工作區應用程式",
  "Apps could not be loaded": "無法載入應用程式",
  "No apps installed yet": "尚未安裝應用程式",
  "Explore apps": "探索應用程式",
  "Use Sign in in the status bar.": "請使用狀態列中的「登入」。",
  "Apps add new skills to your workspace — a customer list, social posts and more. Browse what's available and install one in a tap.": "應用程式可為工作區加入新功能，例如客戶清單、社群貼文等。瀏覽可用項目，輕點即可安裝。",
  "Your admin hasn't installed any apps yet. Browse what's available and ask for one.": "管理員尚未安裝任何應用程式。瀏覽可用項目並提出安裝要求。",
  "Your apps didn't load.": "無法載入你的應用程式。",
  "The workspace is busy. Retrying…": "工作區目前忙碌，正在重試…",
  "Loading your apps…": "正在載入你的應用程式…",
  "Favorites": "我的最愛",
  "Installed": "已安裝",
  "Recently removed": "最近移除",
  "Project apps": "專案應用程式",
  "Needs attention": "需要處理",
  "app needs attention": "個應用程式需要處理",
  "apps need attention": "個應用程式需要處理",
  "Access off": "存取權已關閉",
  "Finish setup": "完成設定",
  "Update ready": "更新已就緒",
  "Manage access": "管理存取權",
  "Review update": "檢視更新",
  "An admin needs to handle this": "需要由管理員處理",
  "Favorites could not be loaded.": "無法載入我的最愛。",
  "Loading favorites…": "正在載入我的最愛…",
  "Pin the apps you use every day. Tap the star on any app below and it appears here and in the sidebar.": "將日常使用的應用程式釘選在此處。點選下方應用程式旁的星號，即可在此處和側邊欄中查看。",
  "Search your apps": "搜尋你的應用程式",
  "Search installed apps": "搜尋已安裝的應用程式",
  "Recently used": "最近使用",
  "Needs attention first": "優先顯示需要處理的項目",
  "No installed app matches “": "沒有已安裝的應用程式符合「",
  "” — try another word, or look in ": "」— 請嘗試其他關鍵字，或前往「",
  "Explore": "探索",
  "Find more apps": "尋找更多應用程式",
  "Bookings, reviews, invoices and more, made for small businesses.": "專為小型企業打造的預約、評論、發票等應用程式。",
  "Removed apps restore for 30 days, then they're deleted for good.": "已移除的應用程式可在 30 天內還原，之後將永久刪除。",
  "Removed": "已移除",
  "Removed · restore window closed": "已移除 · 還原期限已過",
  "Restoring…": "正在還原…",
  "No project apps installed": "尚未安裝專案應用程式",
  "Install a project app": "安裝專案應用程式",
  "Open app": "開啟應用程式",
  "Legacy page": "舊版頁面",
  "Restore didn't finish. Try again in a moment.": "還原未完成，請稍後再試。",
  "activity unavailable": "無法取得活動資訊",
  "reading activity…": "正在讀取活動…",
  "Nothing running yet": "目前沒有執行中的項目",
  "Retry access check": "重試存取權檢查",
  "Access could not be confirmed. Retry the access check before opening an installed app.": "無法確認存取權。請先重試存取權檢查，再開啟已安裝的應用程式。",
  "Checking access…": "正在檢查存取權…",
  "App unavailable": "應用程式無法使用",
  "Sign in as the operator to resolve an installed app.": "請以操作員身分登入，以開啟已安裝的應用程式。",
  "All apps": "所有應用程式",
  "Couldn’t load installed apps": "無法載入已安裝的應用程式",
  "The installed-app list is unavailable.": "目前無法取得已安裝的應用程式清單。",
  "Loading installed apps…": "正在載入已安裝的應用程式…",
  "App not installed": "尚未安裝此應用程式",
  "No active installation with app key “{appKey}” was found.": "找不到應用程式金鑰為「{appKey}」的有效安裝項目。",
  "Choose an installation": "選擇安裝項目",
  "More than one active installation uses app key “{appKey}”. Choose which installation to open.": "有多個有效安裝項目使用應用程式金鑰「{appKey}」。請選擇要開啟的安裝項目。",
  "Kick off": "開始工作",
  "Ask agent": "詢問代理程式",
  "Open pull request": "開啟提取要求",
  "Acceptance": "驗收條件",
  "Agent fenced": "代理程式已隔離",
  "A bound agent is fenced or needs attention. Unfence is on the lane card.": "已綁定的代理程式遭隔離或需要處理。請在工作區卡片上解除隔離。",
  "Kick off blocked": "無法開始工作",
  "Idea decision": "構想決策",
  "complete": "已完成",
  "Add an update or a question…": "新增更新或問題…",
  "Comment": "留言",
  "Comment could not be posted.": "無法發佈留言。",
  "Comments and changes to this issue will appear here.": "此議題的留言與變更將顯示於此。",
  "No activity yet": "尚無活動",
  "Recent activity": "近期活動",
  "Write a comment to preview it.": "撰寫留言以預覽。",
  "Fields": "欄位",
  "loading apps…": "正在載入應用程式…",
  "could not load apps": "無法載入應用程式",
});

export function useLocale(): LocaleContextValue {
  const value = useContext(LocaleContext);
  if (!value) throw new Error("useLocale must be used inside LocaleProvider");
  return value;
}
