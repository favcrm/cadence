import { fixtureRows, parseAppView, type AppViewRow, type AppViewView } from "./contract";

/**
 * The two worked app-views/v1 examples (CAD-861): the same descriptor
 * JSON mirrors contracts/app-views/v1/examples/*.json byte-for-byte —
 * keep them in sync; the contract test parses both files through the
 * same validator. Rows are *synthetic fixtures* supplied by the host,
 * never read from backend records, never shipped inside a descriptor.
 *
 * The Social Content example deliberately names a workspace app that
 * is not "CRM customers" — it demonstrates that one grammar and one
 * renderer cover different Apps without Social gaining an installed
 * record surface it does not have.
 */

const crmDescriptorJson = {
  contract: "app-views/v1",
  app: "crm",
  title: "CRM — customers overview",
  summary:
    "Data-only view declarations a CRM package could ship: a customers table, a customer detail, and a disabled preview of the host's customer form.",
  views: [
    {
      id: "customers",
      title: "Customers",
      kind: "table",
      fields: [
        { id: "name", label: "Name", format: "text" },
        { id: "email", label: "Email", format: "text" },
        { id: "tier", label: "Tier", format: "enum", values: ["member", "vip", "vip_plus"] },
        { id: "visits", label: "Visits", format: "number" },
        { id: "tags", label: "Tags", format: "tags", kind: "list" },
      ],
      columns: [
        { field: "name" },
        { field: "email" },
        { field: "tier", label: "Level" },
        { field: "visits" },
        { field: "tags" },
      ],
    },
    {
      id: "customer-detail",
      title: "Customer detail",
      kind: "detail",
      fields: [
        { id: "name", label: "Name", format: "text" },
        { id: "email", label: "Email", format: "text" },
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
  summary:
    "Data-only view declarations a Social Content package could ship: a reviewed-caption run table, a run detail, and a disabled preview of the host's caption form.",
  views: [
    {
      id: "caption-runs",
      title: "Caption runs",
      kind: "table",
      fields: [
        { id: "subject", label: "Subject", format: "text" },
        { id: "channel", label: "Channel", format: "enum", values: ["instagram", "facebook"] },
        { id: "status", label: "Status", format: "enum", values: ["draft", "in_review", "released"] },
        { id: "caption", label: "Caption", format: "text" },
        { id: "tags", label: "Tags", format: "tags", kind: "list" },
      ],
      columns: [
        { field: "subject" },
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

/** Fixture rows per view id — host-supplied, synthetic, never from
 *  backend records. */
const crmRows: Record<string, Record<string, unknown>[]> = {
  customers: [
    {
      name: "Synthetic Ada Demo",
      email: "ada.demo@example.invalid",
      tier: "vip",
      visits: 12,
      tags: ["lunch", "ramen"],
    },
    {
      name: "Synthetic 嘉欣 Demo",
      email: "kayan.demo@example.invalid",
      tier: "member",
      visits: 3,
      tags: ["dinner"],
    },
    {
      name: "<script>alert(1)</script>",
      email: "inert.demo@example.invalid",
      tier: "member",
      visits: 0,
      tags: [],
    },
  ],
  "customer-detail": [
    {
      name: "Synthetic Ada Demo",
      email: "ada.demo@example.invalid",
      tier: "vip",
      last_visit: "2026-10-02",
      notes: "Synthetic preview notes; no messages sent.",
    },
  ],
};

const socialRows: Record<string, Record<string, unknown>[]> = {
  "caption-runs": [
    {
      subject: "Summer ramen launch",
      channel: "instagram",
      status: "released",
      caption: "夏日限定｜Kura Summer Ramen HK$88，每日限量 40 碗。",
      tags: ["kura", "summer"],
    },
    {
      subject: "Weekend brunch",
      channel: "facebook",
      status: "in_review",
      caption: "Draft caption under review",
      tags: ["brunch"],
    },
    {
      subject: "<img src=x onerror=alert(1)>",
      channel: "instagram",
      status: "draft",
      caption: "Inert markup in a fixture cell renders as text",
      tags: [],
    },
  ],
  "caption-detail": [
    {
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
