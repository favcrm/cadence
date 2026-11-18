// src/tool-policy.ts
async function withToolAuthority(name, authority, context, execute) {
  const refusal = authority.toolRefusal(name);
  if (refusal)
    throw new Error(refusal);
  context.abortSignal?.throwIfAborted();
  await authority.authorizeTool(name);
  context.abortSignal?.throwIfAborted();
  return execute();
}
function authorizeTool(tool, authority) {
  return {
    ...tool,
    execute(input, invocation, context) {
      return withToolAuthority(tool.name, authority, context, () => tool.execute(input, invocation, context));
    }
  };
}
function authorizeTools(group, authority) {
  if (!group.tools)
    return group;
  return {
    ...group,
    tools: group.tools.map((tool) => authorizeTool(tool, authority))
  };
}

// src/testing.ts
function userRecord(id, text) {
  return {
    id,
    conversationId: 1,
    kind: "pi.user",
    model: [{ role: "user", content: text, timestamp: 1000 }]
  };
}
function assistantRecord(id, content) {
  return {
    id,
    conversationId: 1,
    kind: "pi.assistant",
    model: [
      {
        role: "assistant",
        api: "openai-completions",
        provider: "openai",
        model: "synthetic",
        timestamp: 1000,
        stopReason: "stop",
        usage: {
          input: 0,
          output: 0,
          cacheRead: 0,
          cacheWrite: 0,
          totalTokens: 0,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 }
        },
        content
      }
    ]
  };
}
function conversationFixture(stored, live) {
  let committed = false;
  const observers = new Set;
  const messages = async () => {
    const entry = committed ? live() : undefined;
    return entry === undefined ? stored() : [...stored(), entry];
  };
  const view = async () => ({
    entries: await messages(),
    docs: { "pi.live": {} }
  });
  const session = {
    id: "1",
    messages,
    busy: async () => false,
    watch: async () => {
      let value = await view();
      const closed = Promise.withResolvers();
      let ended = false;
      const finish = (end) => {
        if (!ended) {
          ended = true;
          closed.resolve(end);
        }
      };
      return {
        get value() {
          return value;
        },
        closed: closed.promise,
        start(listener) {
          const entry = live();
          if (entry === undefined) {
            finish({ kind: "ended" });
            return;
          }
          committed = true;
          value = { entries: [...stored(), entry], docs: { "pi.live": {} } };
          const change = {
            kind: "value",
            value,
            ops: [["r", value]],
            retired: false
          };
          for (const observer of observers)
            observer.advance(change);
          listener(change).then(() => finish({ kind: "ended" }), (error) => finish({ kind: "listener_error", error }));
        },
        async stop() {
          const end = { kind: "ended" };
          finish(end);
          return end;
        }
      };
    }
  };
  return {
    session: () => session,
    conversation: async () => ({ entries: async () => ({ items: await messages() }) }),
    pending: async () => [],
    pi: async () => ({
      observeConversation: async (_id, observer) => {
        const value = await view();
        observers.add(observer);
        return { value, detach: () => {
          observers.delete(observer);
        } };
      }
    })
  };
}
export {
  assistantRecord,
  authorizeTool,
  authorizeTools,
  conversationFixture,
  userRecord
};
