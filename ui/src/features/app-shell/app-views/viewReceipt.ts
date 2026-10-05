import type { Installation } from "../../workspace-apps/workspaceApps";
import { APP_VIEW_CONTRACT, parseAppView, type AppViewDescriptor, type AppViewView } from "./contract";
import { APP_BINDING_CONTRACT, parseAppBinding, type AppBindingDescriptor, type AppViewBinding, identitySourceKey, isSafeRecordId } from "./appBinding";

const VIEW_FILE = "views/app-views-v1.json";
const BINDING_FILE = "bindings/app-bindings-v1.json";
const SHA256 = /^sha256:[0-9a-f]{64}$/;

/** Parsed client copy of the verified install receipt. The descriptor,
 *  binding and their byte digests remain attached to the bundle pin. */
export interface AppViewReceipt {
  bundleDigest: string;
  descriptorDigest: string | null;
  descriptor: AppViewDescriptor | null;
  bindingDigest: string | null;
  binding: AppBindingDescriptor | null;
  error: string | null;
}

/** The exact verified table/detail pair used by a live view route. */
export interface ResolvedLiveView {
  view: AppViewView;
  binding: AppViewBinding;
  op: "list" | "show";
  recordId: string | null;
  identityField: string | null;
  tableView: AppViewView | null;
  tableBinding: AppViewBinding | null;
  detailViewId: string | null;
  rowNavigationError: string | null;
}

function validDigest(value: unknown): value is string {
  return typeof value === "string" && SHA256.test(value);
}

function invalidReceipt(bundleDigest: unknown): AppViewReceipt {
  return {
    bundleDigest: validDigest(bundleDigest) ? bundleDigest : "",
    descriptorDigest: null,
    descriptor: null,
    bindingDigest: null,
    binding: null,
    error: "The installed view receipt is incomplete or invalid; live reads are disabled.",
  };
}

/** Validate the paired fields served from the verified installation
 *  snapshot. No descriptor/binding is fetched from an app-controlled URL. */
export function installedViewReceipt(installation: Installation): AppViewReceipt | null {
  const descriptorRaw = installation.view_descriptor;
  const descriptorDigestRaw = installation.view_descriptor_digest;
  const bindingRaw = installation.view_binding;
  const bindingDigestRaw = installation.view_binding_digest;
  const hasDescriptor = descriptorRaw !== null && descriptorRaw !== undefined;
  const hasBinding = bindingRaw !== null && bindingRaw !== undefined;
  const hasAnyReceipt = hasDescriptor || hasBinding || descriptorDigestRaw != null || bindingDigestRaw != null;
  const files = installation.files;
  if (!hasAnyReceipt && Array.isArray(files) && !files.includes(VIEW_FILE) && !files.includes(BINDING_FILE)) {
    return null;
  }
  try {
    if (!validDigest(installation.digest)) throw new Error("invalid bundle digest");
    if (!Array.isArray(files) || files.some((file) => typeof file !== "string")
      || new Set(files).size !== files.length) throw new Error("invalid installed file receipt");
    const hasViewFile = files.filter((file) => file === VIEW_FILE).length === 1;
    const hasBindingFile = files.filter((file) => file === BINDING_FILE).length === 1;
    if (hasDescriptor !== (descriptorDigestRaw != null) || hasDescriptor !== hasViewFile) {
      throw new Error("descriptor receipt does not match its pinned file");
    }
    if (hasBinding !== (bindingDigestRaw != null) || hasBinding !== hasBindingFile) {
      throw new Error("binding receipt does not match its pinned file");
    }
    if (hasBinding && !hasDescriptor) throw new Error("binding has no paired descriptor");
    if (!hasDescriptor && !hasBinding) throw new Error("view file has no verified receipt");
    if (!validDigest(descriptorDigestRaw) && hasDescriptor) throw new Error("invalid descriptor digest");
    if (!validDigest(bindingDigestRaw) && hasBinding) throw new Error("invalid binding digest");

    const descriptor = hasDescriptor ? parseAppView(descriptorRaw) : null;
    if (descriptor && descriptor.app !== installation.name) throw new Error("descriptor app differs from installation");
    const binding = hasBinding && descriptor ? parseAppBinding(bindingRaw, descriptor) : null;
    if (binding && binding.contract !== APP_BINDING_CONTRACT) throw new Error("unexpected binding contract");
    if (descriptor && descriptor.contract !== APP_VIEW_CONTRACT) throw new Error("unexpected descriptor contract");
    return {
      bundleDigest: installation.digest,
      descriptorDigest: hasDescriptor ? descriptorDigestRaw as string : null,
      descriptor,
      bindingDigest: hasBinding ? bindingDigestRaw as string : null,
      binding,
      error: null,
    };
  } catch {
    return invalidReceipt(installation.digest);
  }
}

function bindingFor(receipt: AppViewReceipt, view: AppViewView): AppViewBinding | null {
  if (!receipt.binding) return null;
  return receipt.binding.bindings.find((item) => item.view === view.id) ?? null;
}

function tableIdentityField(table: AppViewView, binding: AppViewBinding): string | null {
  const key = identitySourceKey(binding.source);
  const matches = binding.fields.filter((field) => field.key === key && field.format === "text");
  const descriptorFields = new Map((table.fields ?? []).map((field) => [field.id, field]));
  if (matches.length !== 1 || descriptorFields.get(matches[0].field)?.format !== "text") return null;
  return matches[0].field;
}

function detailForTable(receipt: AppViewReceipt, tableBinding: AppViewBinding): AppViewView[] {
  if (!receipt.descriptor || !receipt.binding) return [];
  return receipt.descriptor.views.filter((candidate) => {
    if (candidate.kind !== "detail") return false;
    const candidateBinding = bindingFor(receipt, candidate);
    return candidateBinding?.source === tableBinding.source && candidateBinding.ops.includes("show");
  });
}

/** Resolve a URL selector against one already-verified paired receipt.
 *  Forms, unknown ids, missing ops/scopes and ambiguous detail/identity
 *  relationships fail closed instead of falling through to the native outlet. */
export function resolveLiveView(
  receipt: AppViewReceipt | null,
  requestedViewId: string | null,
  recordId: string | null,
): { ok: true; route: ResolvedLiveView } | { ok: false; reason: string } {
  if (requestedViewId === null || requestedViewId === "") {
    return { ok: false, reason: "The live view route has no declared view id." };
  }
  if (!receipt || receipt.error || !receipt.descriptor || !receipt.binding
      || !receipt.descriptorDigest || !receipt.bindingDigest) {
    return { ok: false, reason: receipt?.error ?? "This installation has no verified live-read binding." };
  }
  const view = receipt.descriptor.views.find((item) => item.id === requestedViewId);
  if (!view) return { ok: false, reason: "This view is not declared by the installed descriptor." };
  if (view.kind === "form") return { ok: false, reason: "Form previews are disabled and cannot read or submit live data." };
  const binding = bindingFor(receipt, view);
  if (!binding) return { ok: false, reason: "This declared view has no installed read binding." };

  if (view.kind === "table") {
    if (recordId !== null) return { ok: false, reason: "A table route cannot carry a record id." };
    if (!binding.ops.includes("list")) return { ok: false, reason: "This table binding does not admit list reads." };
    if (!(view.columns ?? []).some((column) => binding.fields.some((field) => field.field === column.field))) {
      return { ok: false, reason: "This table has no visible column mapped to the verified source." };
    }
    const identityField = tableIdentityField(view, binding);
    const details = detailForTable(receipt, binding);
    const rowNavigationError = identityField === null
      ? "Rows cannot be opened because this table has no unique bound source identity field."
      : details.length !== 1
        ? "Rows cannot be opened because the matching detail view is missing or ambiguous."
        : null;
    const detail = details.length === 1 ? details[0] : null;
    const detailBinding = detail ? bindingFor(receipt, detail) : null;
    return {
      ok: true,
      route: {
        view,
        binding,
        op: "list",
        recordId: null,
        identityField,
        tableView: view,
        tableBinding: binding,
        detailViewId: detail?.id ?? null,
        rowNavigationError: rowNavigationError ?? (detailBinding ? null : "The matching detail binding is incomplete."),
      },
    };
  }

  if (!isSafeRecordId(recordId)) return { ok: false, reason: "A detail route needs a safe record id." };
  if (!binding.ops.includes("show")) return { ok: false, reason: "This detail binding does not admit show reads." };
  const tables = receipt.descriptor.views.filter((candidate) => {
    if (candidate.kind !== "table") return false;
    const candidateBinding = bindingFor(receipt, candidate);
    return candidateBinding?.source === binding.source && candidateBinding.ops.includes("list")
      && tableIdentityField(candidate, candidateBinding) !== null;
  });
  if (tables.length !== 1) {
    return { ok: false, reason: "This detail has no unique matching table and bound row identity." };
  }
  const tableView = tables[0];
  const tableBinding = bindingFor(receipt, tableView)!;
  const details = detailForTable(receipt, tableBinding);
  if (details.length !== 1 || details[0].id !== view.id) {
    return { ok: false, reason: "This detail is not the unique matching detail for its table." };
  }
  return {
    ok: true,
    route: {
      view,
      binding,
      op: "show",
      recordId,
      identityField: tableIdentityField(tableView, tableBinding),
      tableView,
      tableBinding,
      detailViewId: view.id,
      rowNavigationError: null,
    },
  };
}
