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
