import { chatContext } from "../src/features/app-shell/chatScreen";
import { parseAppChat } from "../src/features/app-shell/chat/contract";
import { chatScreenFor } from "../src/features/app-shell/chatScreen";
import type { Installation } from "../src/features/workspace-apps/workspaceApps";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
const installation = (over: Record<string, unknown>) => ({ name: "social-content", approved: true, executable: true,
  files: ["app.md", "screens/main/screens.json"], ...over }) as unknown as Installation;
const descriptor = parseAppChat({ contract: "app-chat/v1", app: "social-content",
  contexts: [{ id: "main", label: "Social Content", prompts: ["Fetch posts", "Draft a post from the newest", "What's ready to publish?"]}],
  attachments: [], directives: [], subjects: [] });

const screen = chatScreenFor(installation({}), true, "customers", "list");
check(screen === "main", "the Social screen maps to its declared chat context id");
check(JSON.stringify(chatContext(descriptor, screen, false)?.prompts) === JSON.stringify(["Fetch posts", "Draft a post from the newest", "What's ready to publish?"]),
  "the app's declared prompts show for the Social screen");
check(chatScreenFor(installation({}), false, "customers", "list") === null, "an unverified installation matches no chat context");
check(chatScreenFor(installation({ approved: false }), true, "customers", "list") === null, "an unapproved screen matches no chat context");
check(chatScreenFor(installation({ files: ["app.md"] }), true, "customers", "list") === null, "an app with no screen gets no prompts");
check(chatScreenFor(installation({ name: "crm" }), true, "segments", "list") === "segments", "the CRM keeps its section ids");
console.log("social chat prompt checks pass");
