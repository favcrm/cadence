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
