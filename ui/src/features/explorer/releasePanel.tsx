import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { useModal } from "./shared";
import { useTranslate } from "../../lib/locale";
import { ApiError } from "../../lib/api";
import {
  onPrivateReadsPaused,
  onPrivateSessionReset,
  privateReadsAllowed,
  privateSessionGeneration,
  withPrivateOperation,
} from "../../lib/privateSession";
import {
  appExplorer,
  notifyAppsChanged,
  workspaceApps,
  type AccessDiff,
  type CatalogueLocator,
  type CommittedInstallation,
  type InstallCheck,
  type Installation,
  type UpgradeProposal,
} from "../workspace-apps/workspaceApps";
import { appErrorCopy } from "./appErrors";
import type {
  InstallRef,
  PublicApp,
  PublicRelease,
  PublicVersions,
} from "./publicDirectory";
import "./publicDirectory.css";

/**
 * The workspace panel on a published app's detail (CAD-1565): which
 * release is selected, what this workspace has installed, and the
 * check → review → confirm → apply flow for fresh installs and
 * catalogue updates. A Check is read-only (`upload/check` or
 * `upgrade/check`); only an explicit confirmation sends the same
 * `files` map with the checked digest, catalogue locator and CAS pins.
 * An uncertain apply locks the panel until the installation list is
 * read back — it is never guessed or retried.
 */

export type PanelNote = {
  text: string;
  tone: "status" | "alert";
  observed?: { installId: string; version: string };
};

export type Receipt = {
  kind: "install" | "update";
  /** A review cannot be carried into a different signed-in session. */
  sessionGeneration: number;
  release: PublicRelease;
  files: Record<string, string>;
  /** The install this check ran against (null = fresh install). */
  install: InstallRef | null;
  expectedDigest: string | null;
  expectedGeneration: string | null;
  proposal: InstallCheck | UpgradeProposal;
  compatible: boolean;
  secrets: { severity?: string }[];
};

type Flow = { kind: "none" } | { kind: "checked"; receipt: Receipt };

/** Host-scoped unresolved writes outlive routes. Session storage keeps an
 * interrupted document uncertain; it contains identifiers, never package data
 * or credentials. Only operators see this state. */
type OutcomeLock = {
  installId: string | null;
  phase: "applying" | "uncertain";
  target: { version: string; digest: string };
};
const OUTCOME_STORAGE = "cadence:catalogue-unconfirmed";
const outcomeLocks = new Map<string, OutcomeLock>();
const outcomeListeners = new Set<() => void>();
try {
  const saved: unknown = JSON.parse(
    sessionStorage.getItem(OUTCOME_STORAGE) ?? "[]",
  );
  if (Array.isArray(saved)) {
    for (const entry of saved) {
      if (!Array.isArray(entry) || entry.length !== 2) continue;
      const [id, lock] = entry;
      if (
        typeof id !== "string" ||
        !lock ||
        !(lock.installId === null || typeof lock.installId === "string") ||
        typeof lock.target?.version !== "string" ||
        typeof lock.target?.digest !== "string"
      )
        continue;
      outcomeLocks.set(id, {
        installId: lock.installId,
        target: lock.target,
        phase: "uncertain",
      });
    }
  }
} catch {
  /* Storage may be unavailable; in-memory navigation guards still hold. */
}

function subscribeOutcomes(listener: () => void): () => void {
  outcomeListeners.add(listener);
  return () => {
    outcomeListeners.delete(listener);
  };
}

function publishOutcome(id: string, value: OutcomeLock | null): void {
  if (value) outcomeLocks.set(id, value);
  else outcomeLocks.delete(id);
  try {
    if (outcomeLocks.size)
      sessionStorage.setItem(
        OUTCOME_STORAGE,
        JSON.stringify([...outcomeLocks]),
      );
    else sessionStorage.removeItem(OUTCOME_STORAGE);
  } catch {
    /* See the in-memory fallback above. */
  }
  for (const listener of outcomeListeners) listener();
}

function settleOutcome(
  id: string,
  pending: OutcomeLock,
  uncertain: boolean,
): void {
  if (outcomeLocks.get(id) !== pending) return;
  publishOutcome(id, uncertain ? { ...pending, phase: "uncertain" } : null);
}

/** Numeric semver compare; `null` when either side is not `X.Y.Z` —
 * an unordered pair is refused rather than guessed. */
function compareVersions(a: string, b: string): number | null {
  const canonical = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
  if (!canonical.test(a) || !canonical.test(b)) return null;
  const left = a.split(".").map(BigInt);
  const right = b.split(".").map(BigInt);
  for (let i = 0; i < 3; i += 1) {
    if (left[i] > 18446744073709551615n || right[i] > 18446744073709551615n)
      return null;
  }
  for (let i = 0; i < 3; i += 1) {
    const l = left[i];
    const r = right[i];
    if (l !== r) return l < r ? -1 : 1;
  }
  return 0;
}

function locatorEq(
  a: CatalogueLocator | null | undefined,
  b: CatalogueLocator,
): boolean {
  return (
    !!a && a.id === b.id && a.version === b.version && a.digest === b.digest
  );
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function strings(value: unknown): value is string[] {
  return (
    Array.isArray(value) && value.every((item) => typeof item === "string")
  );
}

function slot(value: unknown): boolean {
  return (
    object(value) &&
    value.schema === 1 &&
    typeof value.capability === "string" &&
    value.capability.length > 0 &&
    typeof value.version === "number" &&
    Number.isInteger(value.version) &&
    value.version > 0 &&
    typeof value.action === "string" &&
    typeof value.resource_kind === "string" &&
    typeof value.effect === "string"
  );
}

/** Missing review fields are unavailable, never empty access or implicit compatibility. */
function completeReview(value: unknown): boolean {
  if (
    !object(value) ||
    value.schema !== 1 ||
    value.committed !== false ||
    !strings(value.notes) ||
    !Array.isArray(value.secret_warnings) ||
    !value.secret_warnings.every(object)
  )
    return false;
  const access = value.access_diff;
  if (
    !object(access) ||
    !object(access.connections) ||
    !strings(access.connections.added) ||
    !strings(access.connections.removed) ||
    !object(access.capabilities) ||
    !Object.values(access.capabilities).every(
      (change) =>
        object(change) &&
        (change.before === null || slot(change.before)) &&
        (change.after === null || slot(change.after)),
    )
  )
    return false;
  const compatibility = value.compatibility;
  if (
    !object(compatibility) ||
    compatibility.schema !== 1 ||
    typeof compatibility.declared !== "boolean" ||
    !(typeof compatibility.ok === "boolean" || compatibility.ok === null) ||
    !(
      typeof compatibility.compatible === "boolean" ||
      compatibility.compatible === null
    ) ||
    !Array.isArray(compatibility.checks) ||
    !compatibility.checks.every(
      (check) =>
        object(check) &&
        typeof check.ok === "boolean" &&
        (check.kind === "core"
          ? typeof check.required === "string" && typeof check.host === "string"
          : check.kind === "contract" &&
            typeof check.contract === "string" &&
            Array.isArray(check.required) &&
            check.required.every((major) => Number.isInteger(major)) &&
            Array.isArray(check.supported) &&
            check.supported.every((major) => Number.isInteger(major))),
    )
  )
    return false;
  if ("install_id" in value) {
    const diff = value.structural_diff;
    return (
      object(diff) &&
      strings(diff.added) &&
      strings(diff.changed) &&
      strings(diff.removed)
    );
  }
  return strings(value.files);
}

/** The release document the closed proxy serves: `files` + `expected_digest`,
 * both matching the selected release or the check never happens. */
export async function loadReleaseFiles(
  appId: string,
  release: PublicRelease,
  signal: AbortSignal,
): Promise<Record<string, string>> {
  const query = new URLSearchParams({
    version: release.version,
    digest: release.digest,
  });
  let response: Response;
  try {
    response = await fetch(
      `/api/public-apps/${encodeURIComponent(appId)}/release?${query}`,
      {
        credentials: "same-origin",
        cache: "no-store",
        signal,
        headers: { "X-Cadence-Board": "1" },
      },
    );
  } catch (error) {
    if (signal.aborted) throw error;
    throw new ApiError("The release could not be read.", 502);
  }
  if (response.status === 404 || response.status === 409) {
    throw new ApiError("catalogue release is not installable", response.status);
  }
  if (!response.ok)
    throw new ApiError("The release could not be read.", response.status);
  const body = (await response.json().catch(() => null)) as {
    files?: unknown;
    expected_digest?: unknown;
  } | null;
  if (
    !body ||
    body.expected_digest !== release.digest ||
    !body.files ||
    typeof body.files !== "object" ||
    Array.isArray(body.files)
  ) {
    throw new CheckedMismatch();
  }
  if (
    Object.keys(body.files).length === 0 ||
    !Object.values(body.files).every((text) => typeof text === "string")
  )
    throw new CheckedMismatch();
  return body.files as Record<string, string>;
}

/** One checked apply: upload or upgrade with the exact pins, then refuse
 * unless the committed row and an explicit readback both name the selected
 * release and its catalogue provenance. Throws `CheckedMismatch` when the
 * daemon's records do not match; throws the transport error otherwise. */
export function applyCheckedRelease(
  app: PublicApp,
  release: PublicRelease,
  receipt: Receipt,
): Promise<Installation> {
  return withPrivateOperation(async (operation) => {
    if (receipt.sessionGeneration !== operation.generation)
      throw new CheckedMismatch();
    const locator: CatalogueLocator = {
      id: app.id,
      version: release.version,
      digest: release.digest,
    };
    let row: CommittedInstallation;
    if (receipt.kind === "install") {
      row = await appExplorer.upload(
        {
          files: receipt.files,
          expected_digest: release.digest,
          catalogue: locator,
        },
        operation.signal,
      );
    } else {
      row = await workspaceApps.upgrade(
        receipt.install?.install_id as string,
        {
          files: receipt.files,
          expected_digest: receipt.expectedDigest as string,
          expected_generation: receipt.expectedGeneration as string,
          expected_new_digest: release.digest,
          request_id: `catalogue-${crypto.randomUUID()}`,
          catalogue: locator,
        },
        operation.signal,
      );
    }
    const matches = (record: Installation) =>
      !!record &&
      typeof record.install_id === "string" &&
      record.install_id.length > 0 &&
      typeof record.catalog_generation === "string" &&
      record.catalog_generation.length > 0 &&
      record.storage_kind === "workspace" &&
      !!record.approval &&
      typeof record.approval.state === "string" &&
      record.approval.state.length > 0 &&
      typeof record.approved === "boolean" &&
      typeof record.executable === "boolean" &&
      record.install_id === row.install_id &&
      record.name === app.app &&
      record.version === release.version &&
      record.digest === release.digest &&
      locatorEq(record.catalogue, locator) &&
      (receipt.kind === "install" ||
        record.install_id === receipt.install?.install_id);
    if (row?.committed !== true || !matches(row)) throw new CheckedMismatch();
    let back: Installation;
    try {
      back = await workspaceApps.detail(row.install_id, operation.signal);
    } catch {
      throw new CheckedMismatch();
    }
    if (!matches(back)) throw new CheckedMismatch();
    return back;
  });
}

/** A receipt that does not name the selected release: refuse, never install. */
class CheckedMismatch extends Error {
  constructor() {
    super("The checked package did not match the selected release.");
  }
}

/** The daemon answers a vanished release as 404; a moved workspace as 409.
 * The message heuristics backstop the same refusal on older daemons. */
const WITHDRAWN = /withdrawn|no longer (available|published)|not available/i;
const STALE = /stale|generation|precondition/i;

/** Access lines the reviewed access_diff reports, in plain words. */
function accessLines(diff: AccessDiff | undefined): {
  added: string[];
  removed: string[];
  changed: string[];
} {
  const added = [...(diff?.connections.added ?? [])];
  const removed = [...(diff?.connections.removed ?? [])];
  const changed: string[] = [];
  for (const [slot, row] of Object.entries(diff?.capabilities ?? {})) {
    if (row.before == null && row.after != null) added.push(slot);
    else if (row.before != null && row.after == null) removed.push(slot);
    else changed.push(slot);
  }
  return { added, removed, changed };
}
/** Installed access is local: directory availability never gates these links. */
export function InstalledActions({
  install,
  openHref,
  operator,
}: {
  install: InstallRef;
  openHref: string | null;
  operator: boolean;
}) {
  const t = useTranslate();
  return (
    <>
      {openHref ? (
        <Link className="pub-open" href={openHref}>
          {t("Open the installed app")}
          <span aria-hidden="true"> ↗</span>
        </Link>
      ) : (
        <p className="pub-caption">
          {t("This installation cannot be opened from a link here.")}
        </p>
      )}
      {operator && (
        <Link
          className="pub-open"
          href={`/apps/manage/${encodeURIComponent(install.install_id)}`}
        >
          {t("Manage installation")}
        </Link>
      )}
    </>
  );
}

export default function ReleasePanel({
  app,
  release,
  versions,
  versionsError,
  onRetryVersions,
  install,
  installsReady,
  installsError,
  onRetryInstalls,
  refreshInstalls,
  onApplied,
  openHref,
  operator,
  signedIn,
  chooser,
  installParamMissing,
  onSelectVersion,
}: {
  app: PublicApp;
  /** The exact selected published release; absent metadata never invents one. */
  release: PublicRelease | null;
  versions: PublicVersions | null;
  versionsError: string | null;
  onRetryVersions: () => void;
  install: InstallRef | null;
  installsReady: boolean;
  installsError: string | null;
  onRetryInstalls: () => void;
  /** Re-read the installation list; resolves with the fresh rows. */
  refreshInstalls: () => Promise<InstallRef[] | null>;
  onApplied: (row: Installation) => void;
  openHref: string | null;
  operator: boolean;
  signedIn: boolean;
  /** More than one same-app install and no unambiguous choice — never guess. */
  chooser: InstallRef[] | null;
  /** `?installation=` named an id this workspace does not have. */
  installParamMissing: boolean;
  onSelectVersion: (version: string) => void;
}) {
  const t = useTranslate();
  const serial = useRef(0);
  const mounted = useRef(true);
  const [checking, setChecking] = useState(false);
  const [reconciling, setReconciling] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [flow, setFlow] = useState<Flow>({ kind: "none" });
  const retainedLock = useSyncExternalStore(
    subscribeOutcomes,
    () => outcomeLocks.get(app.id) ?? null,
  );
  const lock = operator ? retainedLock : null;
  const applying = lock?.phase === "applying";
  const [note, setNote] = useState<PanelNote | null>(null);
  const dialogRef = useModal<HTMLDivElement>(confirming, () => {
    if (!applying) setConfirming(false);
  });
  const reviewHeading = useRef<HTMLHeadingElement>(null);
  const outcomeNote = useRef<HTMLParagraphElement>(null);

  const selected = release?.version ?? null;
  const installKey = install?.install_id ?? null;

  // A different selection or a different install clears the check that
  // named the old one; an uncertain lock survives — it is only lifted by
  // an explicit readback.
  useEffect(() => {
    serial.current += 1;
    setChecking(false);
    setReconciling(false);
    setConfirming(false);
    setFlow({ kind: "none" });
    setNote((current) =>
      current?.observed?.installId === installKey &&
      current.observed.version === selected
        ? current
        : null,
    );
  }, [app.id, selected, installKey]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      serial.current += 1;
    };
  }, []);

  useEffect(() => {
    const invalidate = () => {
      serial.current += 1;
      setFlow({ kind: "none" });
      setConfirming(false);
      setChecking(false);
      setReconciling(false);
      setNote(null);
    };
    const stopReset = onPrivateSessionReset(invalidate);
    const stopPause = onPrivateReadsPaused(invalidate);
    return () => {
      stopReset();
      stopPause();
    };
  }, []);

  const relation = ():
    | "missing"
    | "unavailable"
    | "downgrade"
    | "same"
    | "update"
    | "unversioned"
    | "unordered"
    | "install" => {
    if (!release) return "missing";
    if (!release.installable) return "unavailable";
    if (install) {
      if (install.version == null || install.version === "")
        return "unversioned";
      const order = compareVersions(release.version, install.version);
      if (order === null) return "unordered";
      if (order < 0) return "downgrade";
      if (order === 0) return "same";
      return "update";
    }
    return "install";
  };
  const mode = relation();

  const recommended =
    versions?.recommended_version &&
    versions.releases.some(
      (r) => r.version === versions.recommended_version && r.installable,
    )
      ? versions.recommended_version
      : null;

  const proposalMatches =
    flow.kind === "checked" &&
    !!release &&
    flow.receipt.sessionGeneration === privateSessionGeneration() &&
    flow.receipt.proposal.catalogue?.id === app.id &&
    flow.receipt.proposal.name === app.app &&
    flow.receipt.release.version === release.version &&
    flow.receipt.release.digest === release.digest &&
    (flow.receipt.install?.install_id ?? null) === installKey &&
    (flow.receipt.kind === "install" ||
      (flow.receipt.expectedDigest === install?.digest &&
        flow.receipt.expectedGeneration === install?.catalog_generation));

  const isLocked = lock !== null;
  const checkable =
    operator &&
    privateReadsAllowed() &&
    installsReady &&
    !installsError &&
    !chooser &&
    !installParamMissing &&
    versions !== null &&
    !versionsError &&
    !isLocked &&
    !checking &&
    !applying &&
    !!release &&
    release.installable &&
    !!release.digest &&
    (mode === "install" || mode === "update") &&
    (mode === "install" || !!(install?.digest && install?.catalog_generation));

  const offered =
    checkable &&
    proposalMatches &&
    flow.kind === "checked" &&
    flow.receipt.compatible &&
    flow.receipt.secrets.length === 0;

  const status = (): { key: string; text: string } => {
    const recOrder =
      recommended && install?.version
        ? compareVersions(recommended, install.version)
        : null;
    if (isLocked)
      return {
        key: "statusLocked",
        text: applying
          ? t(lock.installId ? "Updating…" : "Installing…")
          : t("Check the installation"),
      };
    if (!installsReady || installsError)
      return {
        key: "",
        text: t(
          installsError
            ? "Installation unavailable"
            : signedIn
              ? "Checking this board…"
              : "Not checked",
        ),
      };
    if (chooser || installParamMissing)
      return { key: "", text: t("Not checked") };
    if (!install) return { key: "", text: t("Not installed") };
    if (
      mode === "downgrade" ||
      mode === "unavailable" ||
      mode === "unordered" ||
      mode === "unversioned"
    )
      return { key: "statusBlocked", text: t("Update blocked") };
    if (mode === "same" && recOrder !== 0)
      return { key: "statusRetained", text: t("Installed version kept") };
    if (recOrder !== null && recOrder > 0) {
      return { key: "statusReady", text: t("Update ready") };
    }
    if (recOrder === 0) {
      return { key: "statusInstalled", text: t("Up to date") };
    }
    return { key: "statusRetained", text: t("Installed version kept") };
  };
  const statusView = status();
  useEffect(() => {
    if (proposalMatches && !checking) reviewHeading.current?.focus();
  }, [proposalMatches, checking]);
  useEffect(() => {
    if (note) outcomeNote.current?.focus();
  }, [note]);

  const startCheck = () => {
    if (
      !checkable ||
      !release ||
      !privateReadsAllowed() ||
      outcomeLocks.has(app.id)
    )
      return;
    const ticket = ++serial.current;
    const epoch = privateSessionGeneration();
    const live = () =>
      mounted.current &&
      serial.current === ticket &&
      privateReadsAllowed() &&
      privateSessionGeneration() === epoch;
    const controller = new AbortController();
    const locator: CatalogueLocator = {
      id: app.id,
      version: release.version,
      digest: release.digest,
    };
    const checkedInstall = install;
    setChecking(true);
    setFlow({ kind: "none" });
    setNote(null);
    void (async () => {
      const files = await loadReleaseFiles(app.id, release, controller.signal);
      if (!live()) return;
      let proposal: InstallCheck | UpgradeProposal;
      let kind: Receipt["kind"];
      if (!checkedInstall) {
        kind = "install";
        proposal = await appExplorer.uploadCheck(
          { files, expected_digest: release.digest, catalogue: locator },
          controller.signal,
        );
      } else {
        kind = "update";
        proposal = await workspaceApps.upgradeCheck(
          checkedInstall.install_id,
          {
            files,
            expected_digest: checkedInstall.digest as string,
            expected_generation: checkedInstall.catalog_generation as string,
            catalogue: locator,
          },
          controller.signal,
        );
      }
      // The receipt must name the exact selected release and the claimed
      // catalogue provenance — a mismatch is malformed, never actionable.
      if (
        proposal.name !== app.app ||
        proposal.version !== release.version ||
        proposal.digest !== release.digest ||
        !locatorEq(proposal.catalogue, locator) ||
        !completeReview(proposal) ||
        (kind === "update" &&
          ((proposal as UpgradeProposal).install_id !==
            checkedInstall?.install_id ||
            (proposal as UpgradeProposal).expected_digest !==
              checkedInstall?.digest ||
            (proposal as UpgradeProposal).expected_generation !==
              checkedInstall?.catalog_generation))
      ) {
        throw new CheckedMismatch();
      }
      const compatible =
        proposal.compatibility?.ok === true &&
        proposal.compatibility.compatible === true;
      const secrets = proposal.secret_warnings as { severity?: string }[];
      if (!live()) return;
      setFlow({
        kind: "checked",
        receipt: {
          kind,
          sessionGeneration: epoch,
          release,
          files,
          install: checkedInstall,
          expectedDigest: checkedInstall?.digest ?? null,
          expectedGeneration: checkedInstall?.catalog_generation ?? null,
          proposal,
          compatible,
          secrets,
        },
      });
      setNote(null);
    })()
      .catch((cause: unknown) => {
        if (!live() || controller.signal.aborted) return;
        if (cause instanceof CheckedMismatch) {
          setNote({
            tone: "alert",
            text: t(
              "The workspace returned an incomplete or mismatched check. Nothing was changed.",
            ),
          });
        } else if (
          cause instanceof ApiError &&
          cause.code === "resource_busy"
        ) {
          setNote({
            tone: "alert",
            text: t(
              "The workspace is busy. The check did not finish; nothing was changed.",
            ),
          });
        } else if (
          cause instanceof ApiError &&
          (cause.status === 401 || cause.status === 403)
        ) {
          setNote({
            tone: "alert",
            text: t("Checking needs the host operator. Nothing was changed."),
          });
        } else if (
          cause instanceof ApiError &&
          ((WITHDRAWN.test(cause.message) && cause.status < 500) ||
            cause.status === 404)
        ) {
          onRetryVersions();
          setNote({
            tone: "alert",
            text: t(
              "{version} is not available to install or update. The installation was not changed.",
            ).replace("{version}", release.version),
          });
        } else if (
          cause instanceof ApiError &&
          (cause.status === 409 ||
            (STALE.test(cause.message) && cause.status < 500))
        ) {
          void refreshInstalls();
          setNote({
            tone: "alert",
            text: t(
              "This check no longer matches the installation. Nothing was changed. Check the selected release again.",
            ),
          });
        } else {
          setNote({
            tone: "alert",
            text: t(
              appErrorCopy(
                cause,
                "The check did not finish. The installation was not changed.",
              ),
            ),
          });
        }
      })
      .finally(() => {
        if (live()) setChecking(false);
      });
  };

  const startApply = () => {
    if (
      !offered ||
      flow.kind !== "checked" ||
      !release ||
      !privateReadsAllowed() ||
      outcomeLocks.has(app.id) ||
      flow.receipt.sessionGeneration !== privateSessionGeneration()
    )
      return;
    const receipt = flow.receipt;
    const ticket = ++serial.current;
    const epoch = privateSessionGeneration();
    const currentSession = () =>
      privateReadsAllowed() && privateSessionGeneration() === epoch;
    const live = () =>
      mounted.current && serial.current === ticket && currentSession();
    const pending: OutcomeLock = {
      installId: receipt.install?.install_id ?? null,
      phase: "applying",
      target: { version: release.version, digest: release.digest },
    };
    publishOutcome(app.id, pending);
    setConfirming(false);
    setNote(null);
    const fail = (text: string) => {
      setFlow({ kind: "none" });
      setNote({ tone: "alert", text });
    };
    void (async () => {
      const row = await applyCheckedRelease(app, release, receipt);
      settleOutcome(app.id, pending, false);
      // The destination/sidebar must refresh even if this route unmounted.
      // Never publish old-session installation data into a new private session.
      if (currentSession()) notifyAppsChanged();
      if (!live()) return;
      setFlow({ kind: "none" });
      setNote({
        tone: "status",
        observed: { installId: row.install_id, version: row.version },
        text: t(
          receipt.kind === "install"
            ? "Installed package {version}. Connections and execution keep their own approval checks."
            : "Updated package to {version}. Connections and execution keep their own approval checks.",
        ).replace("{version}", row.version),
      });
      onApplied(row);
    })()
      .catch((cause: unknown) => {
        const busy =
          cause instanceof ApiError && cause.code === "resource_busy";
        const withdrawn =
          cause instanceof ApiError &&
          ((WITHDRAWN.test(cause.message) && cause.status < 500) ||
            cause.status === 404);
        const stale =
          cause instanceof ApiError &&
          (cause.status === 409 ||
            (STALE.test(cause.message) && cause.status < 500));
        settleOutcome(app.id, pending, !(busy || withdrawn || stale));
        if (!live()) return;
        if (busy) {
          // Admission refused before staging; the checked proposal stays.
          setNote({
            tone: "alert",
            text: t(
              "The workspace is busy. This apply did not start. Try again when it is ready.",
            ),
          });
        } else if (
          cause instanceof ApiError &&
          (cause.status === 401 || cause.status === 403)
        ) {
          fail(
            t(
              "Only a host operator can apply this package. Check the installation before trying again.",
            ),
          );
        } else if (withdrawn) {
          onRetryVersions();
          fail(
            t(
              "This release is no longer available. It was not applied. Review another release.",
            ),
          );
        } else if (stale) {
          void refreshInstalls();
          fail(
            t(
              "This check no longer matches the installation. Review the selected release again.",
            ),
          );
        } else if (
          cause instanceof ApiError &&
          cause.status < 500 &&
          cause.code !== "daemon_unavailable"
        ) {
          fail(
            t(
              "The workspace refused this apply. Check the installation before trying again.",
            ),
          );
        } else {
          fail(
            t(
              "The workspace did not confirm the result. Check the installation before trying again; this page will not retry.",
            ),
          );
        }
      })
      .finally(() => {
        // A lost continuation must not silently release a pending write.
        settleOutcome(app.id, pending, true);
      });
  };

  const reconcile = () => {
    if (!lock || applying || reconciling || !operator || !privateReadsAllowed())
      return;
    const ticket = ++serial.current;
    const epoch = privateSessionGeneration();
    const live = () =>
      mounted.current &&
      serial.current === ticket &&
      privateReadsAllowed() &&
      privateSessionGeneration() === epoch;
    setReconciling(true);
    setNote(null);
    void (async () => {
      const rows = await refreshInstalls();
      if (rows === null) throw new CheckedMismatch();
      if (!live() || outcomeLocks.get(app.id) !== lock) return;
      const locator = { id: app.id, ...lock.target };
      const matches = lock.installId
        ? rows.filter((row) => row.install_id === lock.installId)
        : rows.filter(
            (row) => row.name === app.app && locatorEq(row.catalogue, locator),
          );
      if (matches.length > 1) throw new CheckedMismatch();
      const found = matches[0];
      if (found && (found.name !== app.app || !found.version))
        throw new CheckedMismatch();
      publishOutcome(app.id, null);
      setFlow({ kind: "none" });
      setNote({
        tone: "status",
        ...(found?.version
          ? {
              observed: { installId: found.install_id, version: found.version },
            }
          : {}),
        text: found?.version
          ? t(
              "The installation record shows {version}. You can review another release when ready.",
            ).replace("{version}", found.version)
          : t(
              "No matching installation was found. Review the workspace before trying again.",
            ),
      });
    })()
      .catch(() => {
        if (!live()) return;
        setNote({
          tone: "alert",
          text: t(
            "The installation could not be checked. Updates stay paused.",
          ),
        });
      })
      .finally(() => {
        if (live()) setReconciling(false);
      });
  };

  const factsAvailable =
    versions === null || versionsError ? "—" : (recommended ?? t("None"));

  return (
    <aside
      className="pub-install-panel"
      aria-label={t("Your workspace")}
      aria-busy={checking || applying || reconciling}
    >
      <h2 className="pub-panel-title">{t("Your workspace")}</h2>
      <span
        className="pub-status"
        {...(statusView.key ? { "data-state": statusView.key } : {})}
      >
        {statusView.text}
      </span>
      <dl className="pub-facts">
        <div>
          <dt>{t("Installed")}</dt>
          <dd>
            {install
              ? (install.version ?? "—")
              : installsReady &&
                  !installsError &&
                  !chooser &&
                  !installParamMissing
                ? t("Not installed")
                : "—"}
          </dd>
        </div>
        <div>
          <dt>{t("Available")}</dt>
          <dd>{factsAvailable}</dd>
        </div>
        {selected && selected !== factsAvailable && (
          <div>
            <dt>{t("Selected version")}</dt>
            <dd>{selected}</dd>
          </div>
        )}
      </dl>

      {installParamMissing && (
        <p className="pub-note" role="status">
          {t("The linked installation is not in this workspace.")}
        </p>
      )}
      {chooser && (
        <div className="pub-note" role="status">
          <p>
            {t(
              "This app has more than one installation. Choose which one to review — none is picked for you.",
            )}
          </p>
          <ul className="pub-chooser">
            {chooser.map((row) => (
              <li key={row.install_id}>
                <Link
                  replace
                  href={`/apps/catalog/${encodeURIComponent(app.id)}?${new URLSearchParams({ installation: row.install_id, ...(selected ? { version: selected } : {}) })}`}
                >
                  {row.title} · {row.version ?? "—"}
                  {row.project ? ` · ${row.project}` : ""}
                </Link>
              </li>
            ))}
          </ul>
        </div>
      )}
      {installsError && !installParamMissing && (
        <p className="pub-note" role="alert">
          {t("Installed apps could not be checked. Retry before installing.")}{" "}
          <Button size="sm" onClick={onRetryInstalls}>
            {t("Retry")}
          </Button>
        </p>
      )}
      {signedIn && !installsReady && !installsError && (
        <p className="pub-note" role="status">
          {t("Checking this board…")}
        </p>
      )}
      {versionsError && (
        <p className="pub-note" role="alert">
          {t(versionsError)}{" "}
          <Button size="sm" onClick={onRetryVersions}>
            {t("Retry")}
          </Button>
        </p>
      )}
      {versions === null && !versionsError && (
        <p className="pub-note" role="status">
          {t("Loading published versions…")}
        </p>
      )}
      {note && (
        <p
          ref={outcomeNote}
          tabIndex={-1}
          className="pub-note"
          data-tone={note.tone}
          role={note.tone === "alert" ? "alert" : "status"}
        >
          {note.text}
        </p>
      )}

      {mode === "downgrade" && install?.version && (
        <p className="pub-note">
          {t(
            "{title} {installed} is already installed. An older release cannot replace it. There is no rollback, and this workspace does not update automatically.",
          )
            .replace("{title}", app.title)
            .replace("{installed}", install.version)}
        </p>
      )}
      {mode === "unordered" && install?.version && (
        <p className="pub-note">
          {t(
            "The installed version and this release cannot be ordered. The update is refused rather than guessed.",
          )}
        </p>
      )}
      {mode === "unversioned" && (
        <p className="pub-note">
          {t(
            "The installed version is not readable here, so no catalogue update can be checked.",
          )}
        </p>
      )}
      {mode === "unavailable" && release && (
        <p className="pub-note">
          {t(
            "{version} is not available to install or update. The installation was not changed.",
          ).replace("{version}", release.version)}
        </p>
      )}
      {isLocked && (
        <p className="pub-note" data-tone="alert">
          {t(
            applying
              ? "The package is being applied. Another apply stays blocked even if you leave this page."
              : "The result has not been confirmed. Check the installation before trying again; this page will not guess or retry.",
          )}
        </p>
      )}
      {!operator && signedIn && !chooser && (
        <p className="pub-note">
          {t(
            install
              ? "An operator has to check and apply catalogue updates. You can still open the installed app."
              : "An operator has to install apps. You can browse releases.",
          )}
        </p>
      )}
      {!signedIn && (
        <p className="pub-note">
          <a
            className="btn btn-primary"
            href={`/__platform/login?to=${encodeURIComponent(window.location.href)}`}
          >
            {t("Sign in to install")}
          </a>
        </p>
      )}

      {proposalMatches && flow.kind === "checked" && release && (
        <section
          className="pub-review"
          aria-live="polite"
          data-version={release.version}
          data-digest={release.digest}
        >
          <h2 ref={reviewHeading} tabIndex={-1}>
            {install
              ? t("Review version {version}").replace(
                  "{version}",
                  release.version,
                )
              : t("Review installation of {version}").replace(
                  "{version}",
                  release.version,
                )}
          </h2>
          {release.release_notes && (
            <div>
              <h3>{t("Release notes")}</h3>
              <p>{release.release_notes}</p>
            </div>
          )}
          <AccessReview
            diff={flow.receipt.proposal.access_diff}
            fresh={!install}
            t={t}
          />
          <div>
            <h3>{t("Compatibility")}</h3>
            {!flow.receipt.compatible && (
              <p className="pub-note" data-tone="alert">
                {t(
                  "Not compatible with this workspace. This release cannot be applied.",
                )}
              </p>
            )}
            <CompatibilityLines
              proposal={flow.receipt.proposal}
              compatible={flow.receipt.compatible}
              t={t}
            />
          </div>
          {!!flow.receipt.proposal.notes?.length && (
            <div>
              <h3>{t("Notes")}</h3>
              <ul>
                {flow.receipt.proposal.notes.map((item, i) => (
                  <li key={i}>{item}</li>
                ))}
              </ul>
            </div>
          )}
          {flow.receipt.secrets.length > 0 && (
            <p className="pub-note" data-tone="alert">
              {t(
                "Package validation reported secret warnings; this release cannot be applied from here.",
              )}
            </p>
          )}
          {flow.receipt.kind === "update" && (
            <div>
              <h3>{t("Package changes")}</h3>
              <ul>
                {(["added", "changed", "removed"] as const).map((kind) => (
                  <li key={kind}>
                    {t(
                      kind === "added"
                        ? "Added"
                        : kind === "removed"
                          ? "Removed"
                          : "Changed",
                    )}
                    :{" "}
                    {(flow.receipt.proposal as UpgradeProposal).structural_diff[
                      kind
                    ].join(", ") || t("none")}
                  </li>
                ))}
              </ul>
            </div>
          )}
          <details className="pub-package">
            <summary>{t("Package details")}</summary>
            <p className="pub-digest">
              {t("Digest")}: {release.digest}
            </p>
            <p className="pub-digest">
              {t("Catalogue")}: {app.id}
            </p>
          </details>
        </section>
      )}

      {!chooser && (
        <div className="pub-actions">
          {offered && flow.kind === "checked" && release && (
            <Button variant="primary" onClick={() => setConfirming(true)}>
              {install
                ? t("Update to {version}").replace("{version}", release.version)
                : t("Install {version}").replace("{version}", release.version)}
            </Button>
          )}
          {operator &&
            !isLocked &&
            release &&
            (mode === "install" || mode === "update") && (
              <Button
                variant={proposalMatches ? "secondary" : "primary"}
                disabled={!checkable}
                onClick={startCheck}
              >
                {checking
                  ? t("Checking…")
                  : proposalMatches
                    ? t("Check again")
                    : install
                      ? t("Review update")
                      : t("Review installation")}
              </Button>
            )}
          {isLocked && operator && (
            <Button onClick={reconcile} disabled={reconciling || applying}>
              {reconciling
                ? t("Checking the installation…")
                : t("Check installation")}
            </Button>
          )}
          {install && (
            <InstalledActions
              install={install}
              openHref={openHref}
              operator={operator}
            />
          )}
        </div>
      )}

      <p className="pub-caption">
        {t(
          "Checks published releases. Your installed app only changes after you confirm an update. Git and local sources stay on the app's manage page.",
        )}
      </p>

      {versions && versions.releases.length > 0 && (
        <details className="pub-versions">
          <summary>{t("Other versions")}</summary>
          <fieldset
            className="pub-version-list"
            disabled={applying || reconciling}
          >
            <legend className="sr-only">{t("Other versions")}</legend>
            {versions.releases.map((item) => {
              const marks: string[] = [];
              if (recommended === item.version) marks.push(t("Recommended"));
              if (install?.version === item.version) marks.push(t("Installed"));
              if (
                install?.version &&
                item.installable &&
                compareVersions(item.version, install.version) === -1
              )
                marks.push(t("Older eligible release"));
              if (!item.installable) marks.push(t("Unavailable"));
              const checked = item.version === selected;
              return (
                <label className="pub-version" key={item.version}>
                  <input
                    type="radio"
                    name={`release-${app.id}`}
                    value={item.version}
                    checked={checked}
                    onChange={() => onSelectVersion(item.version)}
                  />
                  <span>
                    <b>{item.version}</b>
                    <small>{marks.join(" · ")}</small>
                  </span>
                </label>
              );
            })}
          </fieldset>
        </details>
      )}

      {confirming && offered && flow.kind === "checked" && release && (
        <div
          className="scrim fixed inset-0 bg-black/50 z-50 grid place-items-center p-4"
          role="presentation"
          onClick={() => {
            if (!applying) setConfirming(false);
          }}
        >
          <div
            ref={dialogRef}
            tabIndex={-1}
            className="modal card p-5 max-w-lg w-full"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="pub-confirm-title"
            aria-describedby="pub-confirm-body"
            onClick={(e) => e.stopPropagation()}
          >
            <h2
              id="pub-confirm-title"
              className="text-cardtitle font-medium text-ink-100 mb-3"
            >
              {install
                ? t("Update {title} to {version}?")
                    .replace("{title}", app.title)
                    .replace("{version}", release.version)
                : t("Install {title} {version}?")
                    .replace("{title}", app.title)
                    .replace("{version}", release.version)}
            </h2>
            <div id="pub-confirm-body">
              <p className="text-label text-ink-400 mb-2">
                {install
                  ? t(
                      "Update the package from {from} to {version}. Connections and execution keep their own approval checks. Later releases will not install automatically.",
                    )
                      .replace("{from}", install.version ?? "—")
                      .replace("{version}", release.version)
                  : t(
                      "Install exactly package {version}. Connections and execution need their own approval checks. Later releases will not install automatically.",
                    ).replace("{version}", release.version)}
              </p>
              <p className="pub-digest mb-4">{release.digest}</p>
            </div>
            <div className="flex gap-2 justify-end">
              <Button onClick={() => setConfirming(false)} disabled={applying}>
                {t("Cancel")}
              </Button>
              <Button
                className="btn-primary"
                disabled={applying}
                onClick={startApply}
              >
                {applying
                  ? install
                    ? t("Updating…")
                    : t("Installing…")
                  : install
                    ? t("Update to {version}").replace(
                        "{version}",
                        release.version,
                      )
                    : t("Install {version}").replace(
                        "{version}",
                        release.version,
                      )}
              </Button>
            </div>
          </div>
        </div>
      )}
    </aside>
  );
}

/** The reviewed access as added/removed/changed slots, never invented. */
function AccessReview({
  diff,
  fresh,
  t,
}: {
  diff: AccessDiff | undefined;
  fresh: boolean;
  t: (s: string) => string;
}) {
  const rows = accessLines(diff);
  const empty =
    rows.added.length === 0 &&
    rows.removed.length === 0 &&
    rows.changed.length === 0;
  return (
    <div>
      <h3>{t("Access")}</h3>
      {empty ? (
        <p>
          {fresh
            ? t("This release declares no access changes to review.")
            : t("No access changes from the installed release.")}
        </p>
      ) : (
        <ul>
          {rows.added.map((slot) => (
            <li key={`a-${slot}`}>
              {t("Added")}: {slot}
            </li>
          ))}
          {rows.changed.map((slot) => (
            <li key={`c-${slot}`}>
              {t("Changed")}: {slot}
            </li>
          ))}
          {rows.removed.map((slot) => (
            <li key={`r-${slot}`}>
              {t("Removed")}: {slot}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The daemon's compatibility receipt as readable lines; an undetermined
 * answer is shown as such and blocks Apply upstream via `compatible`. */
function CompatibilityLines({
  proposal,
  compatible,
  t,
}: {
  proposal: InstallCheck | UpgradeProposal;
  compatible: boolean;
  t: (s: string) => string;
}) {
  const receipt = proposal.compatibility;
  if (!receipt) return <p>{t("No host requirements declared.")}</p>;
  if (!receipt.declared) return <p>{t("No host requirements declared.")}</p>;
  if (receipt.status === "unknown")
    return <p>{t("Compatibility could not be determined.")}</p>;
  if (receipt.checks.length === 0) {
    return (
      <p>
        {compatible
          ? t("Compatible with this workspace.")
          : t("Compatibility requirements could not be read.")}
      </p>
    );
  }
  return (
    <ul>
      {receipt.checks.map((check, index) => {
        if (check.kind === "core") {
          const required =
            typeof check.required === "string"
              ? check.required
              : check.required.join(", ");
          return (
            <li key={`core-${index}`}>
              {t("Needs Cadence {required} — this host {host}")
                .replace("{required}", required)
                .replace("{host}", check.host ?? "unknown")}{" "}
              {check.ok ? "✓" : "✗"}
            </li>
          );
        }
        const required = Array.isArray(check.required)
          ? check.required
          : [check.required];
        const supported = check.supported ?? [];
        return (
          <li key={`contract-${check.contract ?? index}`}>
            {check.ok
              ? t("Needs {contract} {required} — supported")
                  .replace("{contract}", check.contract ?? "host contract")
                  .replace(
                    "{required}",
                    required.map((major) => `v${major}`).join(", "),
                  )
              : t(
                  "Needs {contract} {required} — not supported (host supports {supported})",
                )
                  .replace("{contract}", check.contract ?? "host contract")
                  .replace(
                    "{required}",
                    required.map((major) => `v${major}`).join(", "),
                  )
                  .replace(
                    "{supported}",
                    supported.length
                      ? supported.map((major) => `v${major}`).join(", ")
                      : "none",
                  )}{" "}
            {check.ok ? "✓" : "✗"}
          </li>
        );
      })}
    </ul>
  );
}
