/**
 * The host registries an app-chat descriptor may name (CAD-1109). Ids only:
 * the host implements each once (`actions.ts`, `capabilities.tsx`) and the
 * descriptor can never add one. `contracts/app-chat/v1/app-chat.schema.json`
 * lists the same ids and the daemon's install validator reads them from that
 * file; a test keeps the three equal.
 */
export const HOST_ACTION_IDS = ["open-view"] as const;
export type HostActionId = (typeof HOST_ACTION_IDS)[number];

export const HOST_ATTACHMENT_IDS = ["csv-import"] as const;
export type HostAttachmentId = (typeof HOST_ATTACHMENT_IDS)[number];
