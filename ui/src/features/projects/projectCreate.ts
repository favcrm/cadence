import { ApiError } from "../../lib/apiError";
import type { Project, ProjectPreset } from "../../lib/types";

export interface ProjectPresetOption {
  value: ProjectPreset;
  label: string;
  hint: string;
}

export const PROJECT_PRESETS: readonly ProjectPresetOption[] = [
  {
    value: "general",
    label: "General",
    hint: "Cross-functional work, initiatives, and deliverables",
  },
  {
    value: "client_delivery",
    label: "Client Delivery",
    hint: "Client engagements, milestones, and handoffs",
  },
  {
    value: "marketing",
    label: "Marketing",
    hint: "Campaigns, content calendars, and launches",
  },
  {
    value: "software",
    label: "Software",
    hint: "Engineering tasks, repositories, and releases",
  },
  {
    value: "operations",
    label: "Operations",
    hint: "Recurring workflows, compliance, and internal ops",
  },
];

/** Issue prefix syntax from cadence-core's `check_prefix`. */
export const PROJECT_PREFIX = /^[A-Z][A-Z0-9]{0,7}$/;

/** Derive a lowercase project key slug from a human-readable project name. */
export function slugifyProjectKey(name: string): string {
  const collapsed = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  if (collapsed.length <= 32) return collapsed;
  return collapsed.slice(0, 32).replace(/-+$/g, "");
}

/** Derive a 1-3 character uppercase ID prefix from a project key, or "" if invalid. */
export function deriveProjectPrefix(key: string): string {
  const prefix = key
    .replace(/[^a-zA-Z0-9]/g, "")
    .slice(0, 3)
    .toUpperCase();
  return PROJECT_PREFIX.test(prefix) ? prefix : "";
}

/** Resolve display title for a project, falling back to its key. */
export function projectDisplayName(project: Pick<Project, "key" | "name">): string {
  const name = project.name?.trim();
  return name && name.length > 0 ? name : project.key;
}

/** Resolve the English label for a project preset key, or null when unset/unknown. */
export function projectPresetLabel(preset: string | null | undefined): string | null {
  const trimmed = preset?.trim();
  if (!trimmed) return null;
  return PROJECT_PRESETS.find((option) => option.value === trimmed)?.label ?? null;
}

function readErrorCode(err: unknown): string | undefined {
  if (err instanceof ApiError) {
    return err.code ?? err.check;
  }
  if (err !== null && typeof err === "object") {
    const rec = err as Record<string, unknown>;
    if (typeof rec.code === "string" && rec.code.length > 0) return rec.code;
    if (typeof rec.check === "string" && rec.check.length > 0) return rec.check;
    if (rec.error !== null && typeof rec.error === "object") {
      const nested = rec.error as Record<string, unknown>;
      if (typeof nested.code === "string" && nested.code.length > 0) return nested.code;
    }
  }
  return undefined;
}

function readErrorMessage(err: unknown): string | undefined {
  if (err instanceof Error && err.message) return err.message;
  if (err !== null && typeof err === "object") {
    const rec = err as Record<string, unknown>;
    if (typeof rec.error === "string" && rec.error.length > 0) return rec.error;
    if (rec.error !== null && typeof rec.error === "object") {
      const nested = rec.error as Record<string, unknown>;
      if (typeof nested.message === "string" && nested.message.length > 0) return nested.message;
    }
    if (typeof rec.message === "string" && rec.message.length > 0) return rec.message;
  }
  return undefined;
}

/** Map native or hosted `POST /api/projects` refusals to user-facing copy. */
export function createProjectErrorMessage(
  err: unknown,
  t: (english: string) => string = (s) => s,
): string {
  const code = readErrorCode(err);
  switch (code) {
    case "project_exists":
      return t("A project with this key already exists. Choose a different key in Advanced.");
    case "prefix_conflict":
      return t("That ID prefix is already used by another project. Choose a different prefix in Advanced.");
    case "not_permitted":
    case "owner_required":
    case "operator_only":
    case "member_role":
    case "operator_proof":
      return t("Only the workspace owner or operator can create projects.");
    case "read_only":
      return t("This board is read-only and cannot create projects.");
    case "private_unavailable":
      return t("Private projects are not available on this board yet. Choose Workspace visibility.");
    case "capability_unavailable":
    case "unsupported_daemon":
      return t("Project creation is not supported by this daemon yet.");
    case "daemon_unavailable":
      return t("The local daemon is not reachable. Start it and try again.");
    case "validation_failed":
    case "invalid_request": {
      const msg = readErrorMessage(err);
      if (msg && !/^\d{3}\b/.test(msg)) return msg;
      return t("Check the project details and try again.");
    }
    default:
      break;
  }

  if (err instanceof ApiError && err.status === 403) {
    return t("Only the workspace owner or operator can create projects.");
  }

  const msg = readErrorMessage(err);
  if (msg && !/^(Failed to fetch|NetworkError|Load failed)/i.test(msg)) {
    return msg;
  }
  return t("Could not create project. Check your connection and try again.");
}
