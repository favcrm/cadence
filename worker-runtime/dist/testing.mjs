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
function liveBatch(entry) {
  if (typeof entry !== "object" || entry === null || !("model" in entry)) {
    throw new Error("liveBatch requires a record from assistantRecord");
  }
  const model = entry.model;
  if (!Array.isArray(model) || model.length === 0) {
    throw new Error("liveBatch requires a record with one stored message");
  }
  const message = model[0];
  if (typeof message !== "object" || message === null || !("content" in message) || !Array.isArray(message.content)) {
    throw new Error("liveBatch requires an assistant message with content");
  }
  const changes = [];
  message.content.forEach((block, contentIndex) => {
    if (typeof block === "object" && block !== null && "type" in block && block.type === "thinking" && "thinking" in block && typeof block.thinking === "string") {
      changes.push({
        type: "thinking_delta",
        contentIndex,
        delta: block.thinking
      });
    }
    changes.push({ type: "block", contentIndex, block });
  });
  changes.push({ type: "message", message });
  return [
    {
      type: "message_update",
      usage: "usage" in message ? message.usage : undefined,
      changes
    },
    { type: "message_end", entry }
  ];
}
export {
  assistantRecord,
  authorizeTool,
  authorizeTools,
  liveBatch,
  userRecord
};
