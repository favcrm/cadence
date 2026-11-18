import { ApiError } from "../../lib/api";

export type CatalogEffect = "read" | "draft";
export type CatalogMoney = { currency: "USD"; scale: 6; amount: string };
type CatalogToolBase = {
  slug: string;
  displayName: string;
  effect: CatalogEffect;
  connectedAccounts?: { id: string; provider: string; account: string }[];
};
export type CatalogTool = CatalogToolBase & (
  | { pricing: "fixed"; price: CatalogMoney; unitPrice: CatalogMoney | null }
  | { pricing: "request_quote"; defaultModel: string }
);
export type CatalogGrantReceipt = { id: string; state: "configured" | "revoked"; revision: number };

const SLUG = /^[a-z][a-z0-9_.-]{0,63}$/;
const DIGEST = /^sha256:[a-f0-9]{64}$/;
const ID = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,159}$/;
const MONEY = /^(0|[1-9]\d{0,9})\.\d{6}$/;
const MODEL = /^[A-Za-z0-9][A-Za-z0-9._/-]{1,119}$/;
const object = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const invalid = () => new ApiError("The platform tool response could not be verified.", 502);

function money(value: unknown): value is CatalogMoney {
  return object(value) && value.currency === "USD" && value.scale === 6 &&
    typeof value.amount === "string" && MONEY.test(value.amount);
}

/** Never show or grant a partial or mismatched catalogue answer. */
export function parseCatalogTools(value: unknown, effect: CatalogEffect): CatalogTool[] {
  if (!object(value) || !Array.isArray(value.tools)) throw invalid();
  const seen = new Set<string>();
  return value.tools.map((tool): CatalogTool => {
    if (!object(tool) || typeof tool.slug !== "string" || !SLUG.test(tool.slug) ||
        seen.has(tool.slug) || typeof tool.displayName !== "string" || !tool.displayName ||
        tool.effect !== effect) throw invalid();
    const quote = "pricing" in tool;
    const allowed = quote
      ? ["slug", "displayName", "effect", "defaultModel", "pricing"]
      : ["slug", "displayName", "effect", "price", "unitPrice", "chargePrecondition", "defaultModel", "connected_accounts"];
    if (Object.keys(tool).some(key => !allowed.includes(key))) throw invalid();
    if (quote) {
      if (tool.slug !== "generate_text" || effect !== "draft" ||
          typeof tool.defaultModel !== "string" || !MODEL.test(tool.defaultModel) ||
          !object(tool.pricing) || Object.keys(tool.pricing).sort().join(",") !== "chargeProtocol,mode,quoteVersion" ||
          tool.pricing.mode !== "request_quote" || tool.pricing.quoteVersion !== "text-model-quote@1" ||
          tool.pricing.chargeProtocol !== "quote@2") throw invalid();
    } else if (!money(tool.price) || !(tool.unitPrice === null || money(tool.unitPrice)) ||
        (tool.defaultModel !== undefined && (typeof tool.defaultModel !== "string" || !MODEL.test(tool.defaultModel))) ||
        (tool.chargePrecondition !== undefined && tool.chargePrecondition !== "max_charge_minor@1" &&
          tool.chargePrecondition !== "speech_quote@1" && tool.chargePrecondition !== "quote@2")) throw invalid();
    seen.add(tool.slug);
    let connectedAccounts: CatalogTool["connectedAccounts"];
    if (tool.connected_accounts !== undefined) {
      if (!Array.isArray(tool.connected_accounts)) throw invalid();
      const ids = new Set<string>();
      connectedAccounts = tool.connected_accounts.map((account) => {
        if (!object(account) || typeof account.id !== "string" || !ID.test(account.id) || ids.has(account.id) ||
            typeof account.provider !== "string" || !account.provider ||
            typeof account.account !== "string" || account.account.length > 512) throw invalid();
        ids.add(account.id);
        return { id: account.id, provider: account.provider, account: account.account };
      });
    }
    const base = { slug: tool.slug, displayName: tool.displayName, effect,
      ...(connectedAccounts === undefined ? {} : { connectedAccounts }) };
    return quote
      ? { ...base, pricing: "request_quote", defaultModel: tool.defaultModel as string }
      : { ...base, pricing: "fixed", price: tool.price as CatalogMoney, unitPrice: tool.unitPrice as CatalogMoney | null };
  });
}

/** A write succeeds only when its receipt names the exact selected grant. */
export function parseCatalogGrant(
  value: unknown,
  expected: { installId: string; digest: string; slot: string; slug: string; effect: CatalogEffect; connectionId?: string },
): CatalogGrantReceipt {
  if (!object(value) || !object(value.binding)) throw invalid();
  const binding = value.binding;
  if (!object(binding.config)) throw invalid();
  const config = binding.config;
  if (!object(config.declaration)) throw invalid();
  const declaration = config.declaration;
  if (!SLUG.test(expected.slug) || !DIGEST.test(expected.digest) ||
      typeof binding.id !== "string" || !binding.id || binding.install_id !== expected.installId ||
      binding.context_id !== null || binding.slot !== expected.slot ||
      (binding.state !== "configured" && binding.state !== "revoked") ||
      typeof binding.revision !== "number" || !Number.isSafeInteger(binding.revision) || binding.revision < 1 ||
      typeof binding.digest !== "string" || !DIGEST.test(binding.digest) ||
      Object.keys(config).sort().join(",") !== (expected.connectionId === undefined
        ? "bundle_digest,connection_kind,declaration,install_id,provider,schema,tool_slug"
        : "bundle_digest,connection_id,connection_kind,declaration,install_id,provider,schema,tool_slug") ||
      Object.keys(declaration).sort().join(",") !== "action,capability,effect,resource_kind,schema,version" ||
      config.schema !== 1 || config.install_id !== expected.installId || config.bundle_digest !== expected.digest ||
      config.connection_kind !== "platform" || config.provider !== "agenticos" || config.tool_slug !== expected.slug ||
      (expected.connectionId !== undefined && (!ID.test(expected.connectionId) || config.connection_id !== expected.connectionId)) ||
      declaration.schema !== 1 || declaration.capability !== "catalog.tool" || declaration.version !== 1 ||
      declaration.action !== "invoke" || declaration.resource_kind !== "connection_account" || declaration.effect !== expected.effect)
    throw invalid();
  return { id: binding.id, state: binding.state, revision: binding.revision };
}
