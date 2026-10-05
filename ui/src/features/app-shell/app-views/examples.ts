import { fixtureRows, parseAppView, type AppViewRow, type AppViewView } from "./contract";

/** The worked app-views/v1 examples mirror contracts/app-views/v1/examples/*.json.
 *  Preview fixtures stay synthetic; installed live reads use a separately
 *  verified app-bindings/v1 receipt and render only fields that binding maps. */

const crmDescriptorJson = {
  contract: "app-views/v1",
  app: "crm",
  title: "CRM — customers overview",
  summary: "Data-only customers table, customer detail, and disabled create-form preview.",
  views: [
    {
      id: "customers",
      title: "Customers",
      kind: "table",
      fields: [
        { id: "record_id", label: "Record ID", format: "text" },
        { id: "name", label: "Name", format: "text" },
        { id: "email", label: "Email", format: "text" },
        { id: "phone", label: "Phone", format: "text" },
        { id: "source", label: "Source", format: "text" },
        { id: "consent_email", label: "Email consent", format: "enum", values: ["granted", "denied", "unknown"] },
        { id: "tier", label: "Tier", format: "enum", values: ["member", "vip", "vip_plus"] },
        { id: "visits", label: "Visits", format: "number" },
        { id: "tags", label: "Tags", format: "tags", kind: "list" },
      ],
      columns: [
        { field: "name" },
        { field: "email" },
        { field: "phone" },
        { field: "consent_email" },
        { field: "tags" },
      ],
    },
    {
      id: "customer-detail",
      title: "Customer detail",
      kind: "detail",
      fields: [
        { id: "record_id", label: "Record ID", format: "text" },
        { id: "name", label: "Name", format: "text" },
        { id: "email", label: "Email", format: "text" },
        { id: "phone", label: "Phone", format: "text" },
        { id: "source", label: "Source", format: "text" },
        { id: "consent_email", label: "Email consent", format: "enum", values: ["granted", "denied", "unknown"] },
        { id: "consent_sms", label: "SMS consent", format: "enum", values: ["granted", "denied", "unknown"] },
        { id: "tags", label: "Tags", format: "tags", kind: "list" },
        { id: "tier", label: "Tier", format: "enum", values: ["member", "vip", "vip_plus"] },
        { id: "last_visit", label: "Last visit", format: "date" },
        { id: "notes", label: "Notes", format: "text", createView: "customer-form" },
      ],
    },
    {
      id: "customer-form",
      title: "New customer (preview)",
      kind: "form",
      previewOf: [
        { id: "name", label: "Full name", format: "text" },
        { id: "email", label: "Email", format: "text" },
        { id: "tier", label: "Tier", format: "enum", values: ["member", "vip", "vip_plus"] },
        { id: "notes", label: "Notes", format: "text" },
      ],
    },
  ],
} as const;

const socialDescriptorJson = {
  contract: "app-views/v1",
  app: "social-content",
  title: "Social Content — caption runs",
  summary: "The live binding projects metadata only; preview fixtures also demonstrate inert text rendering for package-declared fields.",
  views: [
    {
      id: "caption-runs",
      title: "Caption runs",
      kind: "table",
      fields: [
        { id: "id", label: "Run ID", format: "text" },
        { id: "state", label: "State", format: "enum", values: ["awaiting_approval", "approved", "running", "succeeded", "failed", "cancelled"] },
        { id: "workflow_title", label: "Workflow", format: "text" },
        { id: "snapshot_digest", label: "Snapshot digest", format: "text" },
        { id: "subject", label: "Subject", format: "text" },
        { id: "channel", label: "Channel", format: "enum", values: ["instagram", "facebook"] },
        { id: "status", label: "Status", format: "enum", values: ["draft", "in_review", "released"] },
        { id: "caption", label: "Caption", format: "text" },
        { id: "tags", label: "Tags", format: "tags", kind: "list" },
      ],
      columns: [
        { field: "state" },
        { field: "workflow_title" },
        { field: "subject" },
        { field: "snapshot_digest" },
        { field: "channel" },
        { field: "status" },
        { field: "caption" },
        { field: "tags" },
      ],
    },
    {
      id: "caption-detail",
      title: "Caption run detail",
      kind: "detail",
      fields: [
        { id: "id", label: "Run ID", format: "text" },
        { id: "state", label: "State", format: "enum", values: ["awaiting_approval", "approved", "running", "succeeded", "failed", "cancelled"] },
        { id: "workflow_title", label: "Workflow", format: "text" },
        { id: "snapshot_digest", label: "Snapshot digest", format: "text" },
        { id: "subject", label: "Subject", format: "text" },
        { id: "channel", label: "Channel", format: "enum", values: ["instagram", "facebook"] },
        { id: "status", label: "Status", format: "enum", values: ["draft", "in_review", "released"] },
        { id: "reviewed_at", label: "Reviewed at", format: "datetime" },
        { id: "caption", label: "Caption", format: "text", createView: "caption-form" },
      ],
    },
    {
      id: "caption-form",
      title: "New caption run (preview)",
      kind: "form",
      previewOf: [
        { id: "subject", label: "Subject", format: "text" },
        { id: "channel", label: "Channel", format: "enum", values: ["instagram", "facebook"] },
        { id: "source", label: "Source facts", format: "text" },
        { id: "brand_voice", label: "Brand voice", format: "text" },
      ],
    },
  ],
} as const;

/** Synthetic preview rows only; no live backend record enters this map. */
const crmRows: Record<string, Record<string, unknown>[]> = {
  customers: [
    {
      record_id: "preview-customer-1",
      name: "Synthetic Ada Demo",
      email: "ada.demo@example.invalid",
      phone: "+1 555 0100",
      source: "synthetic preview",
      consent_email: "granted",
      tier: "vip",
      visits: 12,
      tags: ["lunch", "ramen"],
    },
    {
      record_id: "preview-customer-2",
      name: "Synthetic 嘉欣 Demo",
      email: "kayan.demo@example.invalid",
      phone: "+1 555 0101",
      source: "synthetic preview",
      consent_email: "unknown",
      tier: "member",
      visits: 3,
      tags: ["dinner"],
    },
    {
      record_id: "preview-customer-3",
      name: "<script>alert(1)</script>",
      email: "inert.demo@example.invalid",
      phone: "+1 555 0102",
      source: "synthetic preview",
      consent_email: "denied",
      tier: "member",
      visits: 0,
      tags: [],
    },
  ],
  "customer-detail": [
    {
      record_id: "preview-customer-1",
      name: "Synthetic Ada Demo",
      email: "ada.demo@example.invalid",
      phone: "+1 555 0100",
      source: "synthetic preview",
      consent_email: "granted",
      consent_sms: "unknown",
      tags: ["lunch", "ramen"],
      tier: "vip",
      last_visit: "2026-10-02",
      notes: "Synthetic preview notes; no messages sent.",
    },
  ],
};

const socialRows: Record<string, Record<string, unknown>[]> = {
  "caption-runs": [
    {
      id: "preview-run-1",
      state: "succeeded",
      workflow_title: "Synthetic social caption workflow",
      snapshot_digest: `sha256:${"a".repeat(64)}`,
      subject: "Summer ramen launch",
      channel: "instagram",
      status: "released",
      caption: "夏日限定｜Kura Summer Ramen HK$88，每日限量 40 碗。",
      tags: ["kura", "summer"],
    },
    {
      id: "preview-run-2",
      state: "awaiting_approval",
      workflow_title: "Synthetic social caption workflow",
      snapshot_digest: `sha256:${"b".repeat(64)}`,
      subject: "Weekend brunch",
      channel: "facebook",
      status: "in_review",
      caption: "Draft caption under review",
      tags: ["brunch"],
    },
    {
      id: "preview-run-3",
      state: "failed",
      workflow_title: "Synthetic social caption workflow",
      snapshot_digest: `sha256:${"c".repeat(64)}`,
      subject: "<img src=x onerror=alert(1)>",
      channel: "instagram",
      status: "draft",
      caption: "Inert markup in a fixture cell renders as text",
      tags: [],
    },
  ],
  "caption-detail": [
    {
      id: "preview-run-1",
      state: "succeeded",
      workflow_title: "Synthetic social caption workflow",
      snapshot_digest: `sha256:${"a".repeat(64)}`,
      subject: "Summer ramen launch",
      channel: "instagram",
      status: "released",
      reviewed_at: "2026-09-30T09:15:00Z",
      caption: "夏日限定｜Kura Summer Ramen HK$88，每日限量 40 碗。優惠受條款及細則約束。",
    },
  ],
};

export interface AppViewExample {
  /** Stable key used by the dev-preview query param. */
  key: "crm" | "social-content";
  descriptor: ReturnType<typeof parseAppView>;
  rows: Record<string, AppViewRow[]>;
}

function buildExample(key: AppViewExample["key"], raw: unknown, rows: Record<string, Record<string, unknown>[]>): AppViewExample {
  const descriptor = parseAppView(raw);
  const bound: Record<string, AppViewRow[]> = {};
  for (const view of descriptor.views as AppViewView[]) {
    bound[view.id] = view.kind === "form" ? [] : fixtureRows(view, rows[view.id] ?? []);
  }
  return { key, descriptor, rows: bound };
}

export const appViewExamples: Record<AppViewExample["key"], AppViewExample> = {
  crm: buildExample("crm", crmDescriptorJson, crmRows),
  "social-content": buildExample("social-content", socialDescriptorJson, socialRows),
};
