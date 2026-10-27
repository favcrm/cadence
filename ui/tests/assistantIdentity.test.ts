import {
  displayName,
  identityLook,
  messageLabel,
  messagePlaceholder,
  readConversationIdentity,
} from "../src/features/assistant/identity";

function ok(condition: boolean, message: string): void {
  if (!condition) throw new Error(`failed: ${message}`);
}

const coordinator = { name: "Workspace Poppy", tone: "Direct", look: "robot", revision: "workspace-r3" };
const personal = { name: "Poppy", tone: "Warm", look: "designer", revision: 3 };
const state = (actor_identity: unknown) => ({ alias: "master", identity: coordinator, actor_identity });
const translate = (text: string) => text === "Assistant" ? "助理" : text;

// A Home snapshot must not lend its personal identity to an app or Recruit.
{
  const home = state({ scope: "personal", conversation_id: "home-a", identity: personal });
  for (const selected of ["app-a", "recruit-a", null, ""]) {
    const actor = readConversationIdentity(home, selected);
    ok(actor.scope === null && actor.read.state === "unavailable", `foreign Home identity refused for ${selected}`);
  }
  const actor = readConversationIdentity(home, "home-a");
  ok(actor.scope === "personal" && actor.read.state === "ready", "matching Home binds its personal actor");
}

// A present but invalid hosted projection may never fall back to a valid workspace profile.
{
  for (const projection of [null, undefined, {}, { scope: "owner", conversation_id: "app-a", identity: coordinator }]) {
    const actor = readConversationIdentity(state(projection), "app-a");
    ok(actor.scope === null && actor.read.state === "unavailable", "invalid hosted actor does not borrow workspace identity");
  }
  for (const identity of [null, { ...personal, revision: "3" }, { ...personal, look: "not-an-avatar" }]) {
    const actor = readConversationIdentity(state({ scope: "personal", conversation_id: "app-a", identity }), "app-a");
    ok(actor.scope === "personal" && actor.read.state === "unavailable", "malformed private profile stays unavailable");
  }
  const native = readConversationIdentity({ alias: "master", identity: coordinator }, "native-master");
  ok(native.scope === "workspace" && native.read.state === "ready", "absent projection retains the Native coordinator contract");
}

// Names and aliases cannot choose ownership; identical names still represent distinct actors.
{
  const sameName = "Poppy";
  const own = readConversationIdentity(state({ scope: "personal", conversation_id: "app-a", identity: { ...personal, name: sameName } }), "app-a");
  const shared = readConversationIdentity(state({ scope: "workspace", conversation_id: "recruit-a", identity: { ...coordinator, name: sameName } }), "recruit-a");
  ok(own.scope === "personal" && own.read.state === "ready", "master alias does not select workspace scope");
  ok(shared.scope === "workspace" && shared.read.state === "ready", "duplicate display name does not select personal scope");
}

// A chosen literal name is not a translation key, including the old generic label.
{
  for (const name of ["Assistant", "助理", "Poppy & Co."]) {
    const actor = readConversationIdentity(state({ scope: "personal", conversation_id: "app-a", identity: { ...personal, name } }), "app-a");
    ok(displayName(actor.read, actor.scope, translate) === name, "chosen name survives localization verbatim");
    ok(messagePlaceholder(actor.read, actor.scope, translate) === `Message ${name}…`, "composer addresses the same named actor");
    ok(messageLabel(actor.read, actor.scope, translate) === `Message ${name}`, "accessible composer addresses the same named actor");
  }
  const unnamed = readConversationIdentity(state({ scope: "personal", conversation_id: "app-a", identity: { ...personal, name: null } }), "app-a");
  ok(displayName(unnamed.read, unnamed.scope, translate) === "助理", "only the unset fallback is localized");
}

// A rename or legacy master alias must not reroll the committed catalog avatar.
{
  for (const name of ["Poppy", "Nova", null]) {
    const actor = readConversationIdentity(state({ scope: "personal", conversation_id: "app-a", identity: { ...personal, name } }), "app-a");
    for (const alias of ["master", "assistant", "a-new-alias"]) {
      ok(identityLook(actor.read, alias) === "designer", "committed look wins over alias and rename");
    }
  }
}

console.log("assistant identity isolation and literal-name behavior passed");
