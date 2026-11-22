import { translateUiText } from "../src/lib/locale";

const issueStatuses = {
  backlog: "待排程",
  ready: "就緒",
  doing: "進行中",
  review: "審查中",
  blocked: "受阻",
  done: "已完成",
  dropped: "已放棄",
};

for (const [status, expected] of Object.entries(issueStatuses)) {
  const translated = translateUiText(status, "zh-TW");
  if (translated !== expected) {
    throw new Error(`Expected zh-TW issue status ${status} to be ${expected}, got ${translated}`);
  }
}

const permissionLabels = {
  "Reject & reply…": "拒絕並回覆…",
  "Saved rules": "已儲存的規則",
  "Needs your approval": "需要你的核准",
  "Pending permission requests and saved rules": "待審核的權限請求與已儲存的規則",
  "What the / commands do": "查看各個 / 指令的用途",
  "Session state — model, effort, session id": "工作階段狀態 — 模型、思考強度與工作階段編號",
  "Context usage — how much of the window is in use": "脈絡用量 — 目前已使用的脈絡視窗比例",
  "Models the provider offers": "供應商提供的模型清單",
  "Set the model (bare: the provider's list)": "設定模型（不帶參數則列出可用模型）",
  "Thinking levels the provider offers": "供應商提供的思考強度等級",
  "Set the thinking level (bare: levels + current)": "設定思考強度（不帶參數則顯示可用等級與目前設定）",
  "Compact the session context now": "立即壓縮工作階段脈絡",
  "Start fresh — the assistant stops using the messages so far": "重新開始 — 助理不再沿用此前的對話訊息",
  "Interrupt the running turn": "中斷正在執行的回合",
  act: "執行",
  Sending: "傳送中",
  "last known": "最後已知狀態",
  paused: "已暫停",
  "partial answer kept": "已保留部分回覆",
  "Master says:": "主助理說明：",
  "…more": "…展開更多",
  "Enter to send · Shift+Enter for a new line": "按 Enter 傳送 · Shift+Enter 換行",
};

for (const [key, expected] of Object.entries(permissionLabels)) {
  const translated = translateUiText(key, "zh-TW");
  if (translated !== expected) {
    throw new Error(`Expected zh-TW permission label ${key} to be ${expected}, got ${translated}`);
  }
}

const leavePrompt = "You have unsaved changes. Leave this page and discard your local edits?";
if (translateUiText(leavePrompt, "zh-TW") !== "你有未儲存的變更。要離開此頁面並捨棄本機編輯內容嗎？") {
  throw new Error("Expected generic screen leave confirmation to translate in zh-TW");
}

const audioTranslations: Record<string, string> = {
  "Record audio": "即時錄音",
  "Upload audio file": "上載錄音檔",
  "Start recording": "開始錄音",
  "Finish recording": "結束錄音",
  "Use transcript": "完成並使用逐字稿",
  "Speech language hint (optional)": "語音語言提示（選填）",
};

for (const [source, expected] of Object.entries(audioTranslations)) {
  const translated = translateUiText(source, "zh-TW");
  if (translated !== expected) {
    throw new Error(`Expected zh-TW audio string "${source}" to be "${expected}", got "${translated}"`);
  }
}

const approvalGateTranslations: Record<string, string> = {
  Marketing: "市場推廣",
  Design: "設計",
  Business: "商務",
  Operations: "營運",
  Engineering: "工程",
  Workspace: "工作區",
  "Target record": "目標記錄",
  "Execution receipt": "執行回執",
  "Add deny note (optional)": "加入拒絕原因（選填）",
  "Ask for changes…": "要求修改…",
  "Local enqueue command": "本機排入合併佇列指令",
  "Approve change": "核准變更",
  "Publish change": "發佈變更",
  "Approve a change": "核准變更",
  "Publish a change": "發佈變更",
  "older version": "較舊提交版本",
  "Checked at the exact version you are about to approve": "已針對你即將核准的確切版本完成檢查",
  "Reviewed and passed by a second agent": "已由另一位代理審查並通過",
  "verdict not recorded": "未記錄審查結論",
  "no head recorded": "未記錄提交版本",
  "Take it back": "收回核准",
  // Existing shared keys must not be overwritten by CAD-2303 additions.
  Target: "目標日期",
  Receipt: "收據",
  Done: "已完成",
  Kind: "類型",
};

for (const [source, expected] of Object.entries(approvalGateTranslations)) {
  const translated = translateUiText(source, "zh-TW");
  if (translated !== expected) {
    throw new Error(`Expected zh-TW approval string "${source}" to be "${expected}", got "${translated}"`);
  }
}

const hostedMergeGateTranslations: Record<string, string> = {
  "Checks": "檢查",
  "Approved": "已核准",
  "In queue": "佇列中",
  "Merged": "已合併",
  "Ready to merge": "可合併",
  "Needs 1 approval": "需要 1 項核准",
  "Needs approvals": "需要核准",
  "In merge queue": "正在合併佇列",
  "Updated PR commit": "PR 已有新提交",
  "Removed from queue": "已移出佇列",
  "Approve & merge": "核准並合併",
  "Verify & merge": "驗證並合併",
  "Email verification": "電子郵件驗證",
  "Send 6-digit code": "寄送 6 位數驗證碼",
  "6-digit verification code": "6 位數驗證碼",
  "Stop merge": "停止合併",
  "Confirm stop merge": "確認停止合併",
  "Hosted merge gate": "代管合併閘道",
  "Pull request updated — a new commit was pushed.": "拉取請求已更新 — 已推送新提交。",
};

for (const [source, expected] of Object.entries(hostedMergeGateTranslations)) {
  const translated = translateUiText(source, "zh-TW");
  if (translated !== expected) {
    throw new Error(`Expected zh-TW hosted gate string "${source}" to be "${expected}", got "${translated}"`);
  }
}

const canvasBrowserTranslations: Record<string, string> = {
  "Open Browser": "開啟瀏覽器",
  "Canvas": "畫布",
  "Resize canvas": "調整畫布寬度",
  "Ask to change": "要求修改",
  "drafting": "撰寫草稿中",
  "updating": "更新中",
  "updated to": "已更新至",
  "Removing installed apps from a hosted workspace is not available yet.": "目前尚未支援從代管工作區移除已安裝的應用程式。",
};

for (const [source, expected] of Object.entries(canvasBrowserTranslations)) {
  const translated = translateUiText(source, "zh-TW");
  if (translated !== expected) {
    throw new Error(`Expected zh-TW canvas/browser string "${source}" to be "${expected}", got "${translated}"`);
  }
}
