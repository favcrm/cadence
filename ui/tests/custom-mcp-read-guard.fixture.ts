// CAD-2727: Shared AgenticOS v2/Cadence wire fixture. Keep both copies identical.
export const guardedMcpReadRequest = {
  tools: [{ tool_name: "catalog.search", mode: "read" }],
  read_guard: {
    snapshot_revision: 3,
    tool_digest: "sha256:9fb42eb60f399ae3387aee19a198a44146d39913d1635be6a4ad0f86e10cf0df",
    tool_updated_at: 1780000000000,
  },
} as const;
