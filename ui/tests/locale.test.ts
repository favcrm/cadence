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
