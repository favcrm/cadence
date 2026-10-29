//#region src/lib/conditionalGet.ts
/** Bounded validators, scoped to this tab's current operator credential.
* 304 is useful only with a matching in-memory representation; no disk cache.
*/
var ConditionalGet = class {
	request;
	identity;
	scope;
	entries = /* @__PURE__ */ new Map();
	constructor(request = fetch, identity = () => null) {
		this.request = request;
		this.identity = identity;
	}
	async get(path, headers, signal) {
		if (signal?.aborted) throw new DOMException("The operation was aborted", "AbortError");
		const scope = this.identity();
		if (scope !== this.scope) {
			this.entries.clear();
			this.scope = scope;
		}
		const previous = this.entries.get(path);
		const response = await this.request(path, {
			headers: {
				...headers,
				...previous ? { "If-None-Match": previous.etag } : {}
			},
			cache: "no-store",
			signal: signal ?? null
		});
		if (signal?.aborted) throw new DOMException("The operation was aborted", "AbortError");
		if (scope !== this.identity()) throw new Error("Session changed during request; retry with the current session");
		if (response.status === 304 && previous) return {
			response,
			value: previous.value
		};
		if (!response.ok) {
			this.entries.delete(path);
			return { response };
		}
		const value = await response.json();
		if (signal?.aborted) throw new DOMException("The operation was aborted", "AbortError");
		if (scope !== this.identity()) throw new Error("Session changed during response decoding; retry with the current session");
		const etag = response.headers?.get("ETag");
		if (etag && scope === this.identity()) {
			this.entries.delete(path);
			this.entries.set(path, {
				etag,
				value
			});
			if (this.entries.size > 64) this.entries.delete(this.entries.keys().next().value);
		}
		return {
			response,
			value
		};
	}
};
//#endregion
//#region src/lib/sessionKey.ts
/**
* The operator session's second credential (CAD-313, review round 2 of
* PR #249). The board's HttpOnly cookie alone is no session: every
* request also carries `X-Cadence-Session: <key>`. The key is scoped to
* the exact origin, port included — so a server on another port that
* receives the cookie (cookies ignore ports) never sees it, and a
* cross-site page cannot set the header.
*
* It lives in `localStorage` so the same browser (a new tab, a reload,
* or a browser restart) keeps the session the cookie already carries;
* `sessionStorage` is read as a fallback so a tab opened before this
* change still works, and written too so a store that blocks
* `localStorage` degrades to the old per-tab behavior instead of
* signing out. Clearing the key is what ends this browser's session.
*/
var SESSION_KEY_STORE = "cadence.sessionKey";
var entryMemoryKey = null;
function entryMode() {
	try {
		return globalThis.__CADENCE_ENTRY_MODE__ === true;
	} catch {
		return false;
	}
}
function consumeEntryBootstrap() {
	if (!entryMode()) return;
	const scope = globalThis;
	const bootstrap = scope.__CADENCE_ENTRY_BOOTSTRAP__;
	if (bootstrap && typeof bootstrap.key === "string" && /^[0-9a-f]{64}$/.test(bootstrap.key)) entryMemoryKey = bootstrap.key;
	try {
		delete scope.__CADENCE_ENTRY_BOOTSTRAP__;
	} catch {}
}
consumeEntryBootstrap();
function sessionKey() {
	if (entryMode()) return entryMemoryKey;
	for (const kind of ["local", "session"]) try {
		const key = (kind === "local" ? globalThis.localStorage : globalThis.sessionStorage).getItem(SESSION_KEY_STORE);
		if (key) return key;
	} catch {}
	return null;
}
//#endregion
//#region src/lib/privateSession.ts
var generation = 0;
var privateSessionGeneration = () => generation;
new ConditionalGet((input, init) => fetch(input, init), () => `${sessionKey() ?? ""}:${privateSessionGeneration()}`);
//#endregion
//#region src/features/workspace-apps/screen/screenProtocol.ts
/** CAD-1123 HP3 — the closed verb sets. A verb outside its set closes the
*  port. `call` verbs run directly: the install's own config and drafts, and
*  (CAD-1918, operator decision) the app's own publish confirmation and
*  schedule — the human's press inside the app is the one approval, bound by
*  the operator session, the host-held action token and the install-bound
*  stored effect. `slot` verbs wait for a tap on a host-drawn button: a run
*  start, the original download and the image picker. Confirm and schedule
*  also stay slot verbs until every installed app sends them as calls
*  (Social Content 0.12.10 still sends slots). */
var CALL_VERBS = [
	"read.run",
	"context.defaults.save",
	"open-link",
	"screen.drawer.open",
	"screen.drawer.close",
	"social.drafts.list",
	"social.drafts.show",
	"social.drafts.create",
	"social.drafts.update",
	"social.drafts.discard",
	"social.drafts.publish.stage",
	"social.sources.show",
	"social.sources.save",
	"social.destinations.list",
	"publish.destination.use",
	"publish.settings.save",
	"publish.draft.confirm",
	"publish.draft.schedule",
	"publish.draft.reschedule",
	"publish.draft.cancel",
	"media.read",
	"media.publish",
	"media.chat.attach"
];
var SLOT_VERBS = [
	"run.start",
	"publish.draft.confirm",
	"publish.draft.schedule",
	"media.original.download",
	"social.image.pick"
];
/** CAD-1365 — the protected original download slot. Qualified per mount
*  (screen-media.v1 + media.read + live token); never a generic action verb. */
var ORIGINAL_DOWNLOAD_VERB = "media.original.download";
/** CAD-1365 — the media contract's verbs parse on the closed call
*  envelope but are NEVER in the generic advertised set: they are
*  appended per-mount, only to a media-opted-in child whose minted
*  receipt declares them. */
var MEDIA_VERBS = [
	"media.read",
	"media.publish",
	"media.chat.attach"
];
/** The verbs a screen.v2 PUSH advertises in `actions` — media verbs are
*  deliberately absent; the channel appends a mount's declared media
*  rights itself. */
var ACTION_VERBS = [.../* @__PURE__ */ new Set([...CALL_VERBS.filter((v) => !MEDIA_VERBS.includes(v)), ...SLOT_VERBS.filter((v) => v !== ORIGINAL_DOWNLOAD_VERB)])];
var RECORD_CALL_VERBS = [
	"records.list",
	"records.show",
	"records.create",
	"records.update",
	"records.csv.preview",
	"segments.list",
	"segments.show",
	"segments.save",
	"exclusions.list",
	"exclusions.show",
	"exclusions.save",
	"suppressions.list",
	"suppressions.add",
	"audience.preview",
	"audience.prepare",
	"audience.show",
	"content.list",
	"content.show",
	"content.save",
	"content.clone",
	"content.render",
	"proposals.list",
	"proposals.show",
	"proposals.render",
	"proposals.apply",
	"proposals.discard",
	"sender-bindings.list",
	"sender-bindings.show",
	"sender-bindings.save",
	"content.test-prepare",
	"content.send-prepare",
	"sends.prepare",
	"sends.show",
	"sends.list",
	"sends.cancel",
	"smtp.show",
	"origin.show",
	"campaign-work.show",
	"campaign-work.save",
	"screen.preview.show",
	"screen.preview.clear",
	"screen.route.patch",
	"whatsapp.status",
	"whatsapp.connect_session",
	"whatsapp.conversations.list",
	"whatsapp.conversations.show",
	"whatsapp.messages",
	"whatsapp.reply.prepare",
	"whatsapp.reply.status",
	"whatsapp.templates.list",
	"whatsapp.template.show",
	"whatsapp.template.prepare",
	"whatsapp.template.status",
	"whatsapp.broadcasts.list",
	"whatsapp.broadcast.show",
	"whatsapp.broadcast.estimate",
	"whatsapp.broadcast.results",
	"whatsapp.broadcast.draft",
	"whatsapp.broadcast.test.prepare",
	"whatsapp.broadcast.prepare",
	"whatsapp.handoff.set",
	"chat.whatsapp.open",
	"whatsapp.draft.show",
	"whatsapp.draft.discard",
	"works.query",
	"works.get",
	"works.create",
	"works.patch",
	"works.trash",
	"works.restore",
	"works.request_review"
];
var RECORD_SLOT_VERBS = [
	"records.csv.import",
	"records.csv.confirm",
	"content.approve",
	"smtp.manage",
	"smtp.bind",
	"smtp.rebind",
	"smtp.revoke",
	"smtp.test-send",
	"sends.approve",
	"sends.resolve",
	"origin.manage",
	"chat.campaign.open",
	"chat.campaign.brief",
	"whatsapp.reply.send",
	"whatsapp.template.send",
	"whatsapp.broadcast.test.send",
	"whatsapp.broadcast.send",
	"whatsapp.broadcast.stop",
	"whatsapp.handoff.release",
	"chat.whatsapp.brief",
	"works.review",
	"works.export"
];
[...RECORD_CALL_VERBS, ...RECORD_SLOT_VERBS];
var LINK_ID_MAX = 128;
var V2_TOP = [
	"sources",
	"defaults",
	"publish_settings",
	"readiness",
	"actions",
	"run_receipts",
	"original_downloads",
	"media_previews",
	"media_chat_references",
	"route",
	"read_observations",
	"presentation_locale"
];
var V2_RUN = [
	"created",
	"closed",
	"phase",
	"steps",
	"inputs_used",
	"caption_excerpt",
	"artifact_id",
	"review",
	"approved",
	"source_post_id",
	"refusal",
	"image_ref"
];
/** The publish-intents.v1 shape: every screen.v2 field is removed. */
function intentsPush(push) {
	const top = {
		...push,
		v: 1
	};
	for (const key of V2_TOP) delete top[key];
	const out = top;
	out.runs = push.runs.map((run) => {
		const row = {
			...run,
			workflow: { title: run.workflow.title }
		};
		for (const key of V2_RUN) delete row[key];
		return row;
	});
	if (push.publish_intents) out.publish_intents = {
		...push.publish_intents,
		rows: push.publish_intents.rows.map(({ refusal: _r, permalink: _p, ...row }) => row)
	};
	return out;
}
/** The exact CAD-1006 v1 shape for a child that did not opt in: every
*  publish-intents.v1 and screen.v2 field is removed, nothing else changes. */
function legacyPush(push) {
	const { publish_intents: _intents, ...rest } = intentsPush(push);
	return {
		...rest,
		runs: rest.runs.map(({ context_id: _context, ...run }) => run),
		outbox: rest.outbox.map(({ run_id: _run, context_id: _context, ...row }) => row)
	};
}
/** The screen.v2 sections, dropped in this order when the PUSH is over its
*  byte cap: each step keeps the screen, and an absent section reads as
*  unavailable to the frame. */
var omit = (key) => (p) => ({
	...p,
	runs: p.runs.map((r) => {
		const row = { ...r };
		delete row[key];
		return row;
	})
});
var DEGRADE = [
	omit("caption_excerpt"),
	({ sources: _s, ...p }) => p,
	omit("inputs_used"),
	(p) => ({
		...p,
		publish_intents: {
			status: "unavailable",
			withheld: 0,
			rows: []
		}
	}),
	omit("steps"),
	omit("review")
];
function pushBytes(push) {
	return new TextEncoder().encode(JSON.stringify(push)).byteLength;
}
var linkId = (value, required) => typeof value === "string" && value.length <= LINK_ID_MAX && (!required || value.length > 0);
/** The exact PUSH one child receives, or `null` when it cannot be sent (the
*  caller then closes the mount and the board shows its native workspace).
*  The byte cap is checked on THIS shape, so a legacy child is never refused
*  for bytes only an opted-in child would get. For an opted-in child, the
*  linkage ids must meet the app's bounds (fail closed otherwise), and when
*  the intent rows alone push it over the cap they are replaced by an honest
*  `unavailable` block rather than dropping the screen. */
function shapeFor(push, level) {
	const shape = level === true ? "intents" : level === false ? "v1" : level;
	if (shape === "v1") {
		const legacy = legacyPush(push);
		return pushBytes(legacy) <= 131072 ? legacy : null;
	}
	if (!push.publish_intents || !push.runs.every((run) => linkId(run.context_id, false)) || !push.outbox.every((row) => linkId(row.run_id, true) && linkId(row.context_id, false))) return null;
	if (shape === "intents") {
		const intents = intentsPush(push);
		if (pushBytes(intents) <= 131072) return intents;
		const degraded = {
			...intents,
			publish_intents: {
				status: "unavailable",
				withheld: 0,
				rows: []
			}
		};
		return pushBytes(degraded) <= 131072 ? degraded : null;
	}
	if (!push.runs.every((run) => typeof run.phase === "string")) return null;
	let v2 = {
		...push,
		v: 2
	};
	for (const degrade of DEGRADE) {
		if (pushBytes(v2) <= 131072) return v2;
		v2 = degrade(v2);
	}
	return pushBytes(v2) <= 131072 ? v2 : null;
}
//#endregion
//#region src/features/workspace-apps/screen/screenProjection.ts
var ID_MAX = 128;
var isId = (value) => value.length > 0 && value.length <= ID_MAX;
function isZone(timezone) {
	if (timezone.length === 0 || timezone.length > 64) return false;
	try {
		new Intl.DateTimeFormat("en-CA", { timeZone: timezone });
		return true;
	} catch {
		return false;
	}
}
/** Same-scope rows only, joined to an in-scope run under its exact context.
*  Foreign, dangling, malformed and duplicated-id rows are withheld (counted,
*  never sent). Secrets in the intent (grant, approval, key, receipt) are
*  never copied. */
function scopedIntents(read, installId, contextId, runContext, v2) {
	if (!read || read.installId !== installId || read.contextId !== contextId) return {
		status: "loading",
		withheld: 0,
		rows: []
	};
	if (read.status !== "ok") return {
		status: read.status,
		withheld: 0,
		rows: []
	};
	if (read.intents.length > 100) throw new Error("The screen projection exceeds its read-only limit");
	const uses = /* @__PURE__ */ new Map();
	for (const intent of read.intents) uses.set(intent.intent_id, (uses.get(intent.intent_id) ?? 0) + 1);
	const rows = [];
	for (const intent of read.intents) {
		const context = intent.context_id ?? "";
		if (uses.get(intent.intent_id) !== 1 || intent.install_id !== installId || runContext.get(intent.run_id) !== context || ![
			intent.intent_id,
			intent.run_id,
			intent.effect_id,
			intent.destination_id
		].every(isId) || !Number.isSafeInteger(intent.due_epoch) || intent.due_epoch < 0 || !isZone(intent.timezone)) continue;
		rows.push({
			intent_id: intent.intent_id,
			install_id: installId,
			context_id: context,
			run_id: intent.run_id,
			effect_id: intent.effect_id,
			state: intent.state,
			channel: intent.channel,
			destination_id: intent.destination_id,
			due_epoch: intent.due_epoch,
			timezone: intent.timezone,
			...v2 ? present({
				refusal: intentRefusal(intent),
				permalink: instagramLink(intent.permalink)
			}) : {}
		});
	}
	return {
		status: read.intents.length >= 100 ? "truncated" : "ok",
		withheld: read.intents.length - rows.length,
		rows
	};
}
var text = (value, limit = 512) => value.slice(0, limit);
/** Plain frame copy for a refused publish, keyed by the daemon's refusal
*  code. The daemon's detail (provider text, URLs, trace ids, store
*  errors) never reaches the frame: only the code and this host-owned line. */
var REFUSED_COPY = {
	provider_refused: "Instagram refused the post. Nothing was published.",
	image_required: "Instagram needs an image for this post. Nothing was published.",
	send_disabled: "Sending is turned off on this Cadence. Nothing was published.",
	approval_replay: "This post needs approving again. Nothing was published.",
	bad_approval: "This post needs approving again. Nothing was published.",
	grant_exhausted: "Publishing permission has run out. Nothing was published.",
	grant_revoked: "Publishing permission was withdrawn. Nothing was published.",
	grant_window: "Publishing permission is not valid right now. Nothing was published.",
	wrong_destination: "The Instagram account is not available for posting. Nothing was published.",
	bad_destination: "The Instagram account is not available for posting. Nothing was published.",
	not_publishable: "The Instagram account is not available for posting. Nothing was published."
};
var REFUSED_FALLBACK = "The post was refused. Nothing was published.";
/** Held reasons the daemon writes verbatim (social_publish_driver.rs). */
var HELD_COPY = {
	"missed publish window": ["missed_window", "The scheduled time passed before it could be sent. Nothing was sent."],
	"approved material changed since freeze": ["material_changed", "The post changed after it was scheduled. Nothing was sent."]
};
var HELD_FALLBACK = "Held — nothing was sent.";
/** The frame's refusal for one intent: a code from the daemon's `code:`
*  prefix (grammar-checked) and host copy. Never the daemon's detail. */
function intentRefusal(intent) {
	if (intent.state === "refused") {
		const code = /^([a-z][a-z0-9_]{0,63}): /.exec(intent.refusal?.message ?? "")?.[1];
		return code ? {
			code,
			text: REFUSED_COPY[code] ?? REFUSED_FALLBACK
		} : {
			code: "refused",
			text: REFUSED_FALLBACK
		};
	}
	if (intent.state === "held") {
		const known = HELD_COPY[intent.refusal?.message ?? ""];
		return known ? {
			code: known[0],
			text: known[1]
		} : {
			code: "held",
			text: HELD_FALLBACK
		};
	}
	return null;
}
var EXCERPT_MAX = 600;
var RATIONALE_MAX = 280;
var POSTS_MAX = 24;
var STEPS_MAX = 16;
var KEYS_MAX = 16;
/** The wire grammars the package adapter (screen-v2.mjs) checks. */
var CODE = /^[a-z0-9][a-z0-9_.-]{0,63}$/;
var KEY = /^[a-z_][a-z0-9_]{0,63}$/;
var SLOT = /^[a-z][a-z0-9_.-]{0,63}$/;
var HANDLE = /^@?[A-Za-z0-9._]{1,30}$/;
var PHASE_KIND = /^[a-z0-9][a-z0-9._-]{0,63}$/;
/** Drop the keys whose value is `null`/`undefined` — the v2 wire omits
*  what the host cannot state rather than sending a placeholder. */
function present(row) {
	return Object.fromEntries(Object.entries(row).filter(([, value]) => value !== null && value !== void 0));
}
function refusalOf(code, message) {
	const line = message.replace(/\s+/g, " ").trim();
	if (!line) return null;
	return {
		code: CODE.test(code) ? code : "refused",
		text: text(line, RATIONALE_MAX)
	};
}
function httpsOn(value, ok, max = 2048) {
	if (typeof value !== "string" || value.length > max) return null;
	try {
		const url = new URL(value);
		return url.protocol === "https:" && !url.username && !url.password && !url.port && ok(url.hostname) && url.href.length <= max ? url.href : null;
	} catch {
		return null;
	}
}
/** A posted or source permalink: https on instagram.com only. */
var instagramLink = (value) => httpsOn(value, (host) => host === "instagram.com" || host === "www.instagram.com", 512);
/** A source thumbnail: https on the Instagram CDN hosts the frame CSP can admit. */
var instagramThumb = (value) => httpsOn(value, (host) => host.endsWith(".cdninstagram.com") || host.endsWith(".fbcdn.net"));
/** The input names an installed workflow marks `context_default`. */
function contextDefaultKeys(installation, workflow) {
	const keys = /* @__PURE__ */ new Map();
	for (const flow of installation.workflows ?? []) {
		if (workflow !== void 0 && flow.name !== workflow) continue;
		for (const input of flow.inputs ?? []) if (input.context_default && KEY.test(input.name) && !keys.has(input.name) && keys.size < KEYS_MAX) keys.set(input.name, typeof input.default === "string" ? input.default : null);
	}
	return keys;
}
/** The daemon always sends these lists; a partial row degrades to empty. */
var list = (value) => Array.isArray(value) ? value : [];
function phaseOf(run) {
	switch (run.state) {
		case "awaiting_approval":
		case "approved": return "awaiting";
		case "succeeded": return "ready";
		case "failed": return "failed";
		case "cancelled": return "cancelled";
	}
	const kinds = new Map(list(run.snapshot.workflow.steps).map((step) => [step.id, step.kind]));
	const active = list(run.steps).find((step) => step.state === "dispatched") ?? list(run.steps).find((step) => step.state === "pending");
	const kind = active ? kinds.get(active.step_id) ?? "" : "";
	if (kind.startsWith("review")) return "checking";
	return PHASE_KIND.test(kind) ? `working:${kind}` : "working:step";
}
var epoch = (value) => Number.isSafeInteger(value) && value >= 0 ? value : null;
var TERMINAL = /* @__PURE__ */ new Set([
	"succeeded",
	"failed",
	"cancelled"
]);
function runV2(run, installation, extras) {
	const flow = (installation.workflows ?? []).find((value) => !!value.source_digest && value.source_digest === run.snapshot.workflow.source_digest);
	const slots = Object.keys(run.snapshot.capabilities ?? {});
	const effects = slots.map((slot) => installation.capabilities?.[slot]?.effect);
	const steps = list(run.snapshot.workflow.steps);
	const receipts = list(run.steps);
	const reviews = list(run.reviews);
	const kinds = new Map(steps.map((step) => [step.id, step.kind]));
	const approve = reviews.find((review) => review.decision === "approve");
	const lastReview = reviews[reviews.length - 1];
	const artifact = approve ? list(run.artifacts).find((value) => value.digest === approve.artifact_digest) : void 0;
	const excerpt = artifact ? extras?.texts.get(artifact.id) : void 0;
	const used = {};
	if (flow) for (const key of contextDefaultKeys(installation, flow.name).keys()) {
		const value = run.snapshot.inputs?.[key];
		if (typeof value === "string") used[key] = {
			value: text(value),
			origin: text(run.snapshot.input_origins?.[key] ?? "run_override", 32)
		};
	}
	const failedStep = receipts.find((step) => step.state === "failed");
	const refusal = run.state === "failed" ? (lastReview?.decision === "revise" ? refusalOf("review_revise", lastReview.rationale) : null) ?? {
		code: "step_failed",
		text: failedStep ? `Step ${text(failedStep.step_id, 32)} (${text(kinds.get(failedStep.step_id) ?? "step", 64)}) did not finish.` : "The run did not finish."
	} : run.state === "cancelled" ? {
		code: "cancelled",
		text: "The run was cancelled."
	} : null;
	const postId = run.snapshot.source?.post?.id;
	return {
		workflow: present({
			name: flow && SLOT.test(flow.name) ? flow.name : null,
			kind: slots.length > 0 && effects.every((effect) => effect === "read") ? "read" : "draft"
		}),
		fields: present({
			created: epoch(run.created),
			closed: TERMINAL.has(run.state) ? epoch(run.updated) : null,
			phase: phaseOf(run),
			steps: steps.slice(0, STEPS_MAX).map((step) => ({
				id: text(step.id, 64),
				kind: text(step.kind, 64),
				state: text(receipts.find((value) => value.step_id === step.id)?.state ?? "pending", 32)
			})),
			inputs_used: Object.keys(used).length ? used : null,
			caption_excerpt: typeof excerpt === "string" ? text(excerpt, EXCERPT_MAX) : null,
			artifact_id: artifact && isId(artifact.id) ? artifact.id : null,
			review: lastReview ? {
				decision: text(lastReview.decision, 32),
				rationale: text(lastReview.rationale, RATIONALE_MAX)
			} : null,
			approved: run.approval && typeof run.approval.by === "string" && epoch(run.approval.at) !== null ? {
				by_display: run.approval.by === "operator" ? "Operator" : text(run.approval.by, 64),
				at: run.approval.at
			} : null,
			source_post_id: typeof postId === "string" && isId(postId) ? postId : null,
			refusal,
			image_ref: approve?.asset_receipt_id && /^[A-Za-z0-9_-]{1,120}$/.test(run.id) ? `image:${run.id}` : null
		})
	};
}
/** The newest finished read-slot run in scope — whose results are the
*  library. Ordered by `updated` (when the run finished), not `created`
*  (when it was queued): a run queued early but finishing late is the
*  freshest read, and a still-running newer queued run is not yet a
*  source. */
function latestSourceRun(installation, runs) {
	return runs.filter((run) => run.state === "succeeded" && Object.keys(run.snapshot.capabilities ?? {}).some((slot) => installation.capabilities?.[slot]?.effect === "read")).reduce((newest, run) => !newest || (run.updated ?? 0) >= (newest.updated ?? 0) ? run : newest, null);
}
/** One normalized source post, honestly shown (link + time) or withheld. */
function sourcePost(post, seen) {
	const permalink = instagramLink(post.permalink);
	const published = epoch(post.published_at_unix ?? void 0);
	if (typeof post.id !== "string" || !isId(post.id) || seen.has(post.id) || !permalink || published === null) return null;
	seen.add(post.id);
	return present({
		id: post.id,
		caption: text(post.caption ?? "", EXCERPT_MAX),
		published_at: published,
		permalink,
		media_kind: text(post.media_kind ?? "", 32),
		thumb_url: instagramThumb(post.preview_url)
	});
}
/** The library, or `null` (omitted: the frame shows it as unavailable)
*  while no verified read is held. One authoritative latest source
*  read wins across both provenances — a real run-bound read carries
*  `run_id`, a retained standalone tool receipt (CAD-1177) carries
*  `tool_receipt_id` (a real receipt id, never a run). Which one is the
*  library is decided by the read's own freshness clock, not by
*  provenance: the run-bound read keys on the run's `updated` (when the
*  read finished); the standalone receipt keys on the host-stamped
*  `created_at` (when the read was retained). A newer standalone
*  instagram.read therefore supersedes an older historical run after
*  reload — the frame always shows the posts the user last fetched,
*  never a stale source that merely ran a workflow earlier. */
function sourcesOf(installation, scopedRuns, extras) {
	const postsFrom = (posts) => {
		const seen = /* @__PURE__ */ new Set();
		const out = [];
		for (const post of list(posts)) {
			if (out.length >= POSTS_MAX) break;
			const row = sourcePost(post, seen);
			if (row) out.push(row);
		}
		return out;
	};
	const latest = latestSourceRun(installation, scopedRuns);
	const runReceipt = latest && extras?.source && extras.source.runId === latest.id ? extras.source.receipts.find((v) => v.run_id === latest.id && v.result?.kind === "social.source.posts") : void 0;
	const runRead = latest && runReceipt && typeof runReceipt.result.handle === "string" && HANDLE.test(runReceipt.result.handle) ? {
		handle: runReceipt.result.handle,
		run: latest,
		when: epoch(latest.updated),
		posts: postsFrom(runReceipt.result.posts)
	} : null;
	const toolRead = extras?.tools ? extras.tools.filter((r) => r.install_id === installation.install_id && installation.capabilities?.[r.slot]?.effect === "read" && r.result?.kind === "social.source.posts" && typeof r.result.handle === "string" && HANDLE.test(r.result.handle)).sort((a, b) => (b.created_at ?? 0) - (a.created_at ?? 0))[0] ?? null : null;
	if (toolRead && (!runRead || (toolRead.created_at ?? 0) > (runRead.when ?? 0)) && toolRead) return present({
		handle: toolRead.result.handle,
		tool_receipt_id: toolRead.id,
		fetched_at: epoch(toolRead.created_at),
		posts: postsFrom(toolRead.result.posts)
	});
	if (runRead) return present({
		handle: runRead.handle,
		run_id: runRead.run.id,
		fetched_at: runRead.when,
		posts: runRead.posts
	});
	return null;
}
function defaultsOf(installation, context) {
	const out = {
		context_id: context?.id ?? "",
		revision: context?.revision ?? 0,
		values: {}
	};
	for (const [key, appDefault] of contextDefaultKeys(installation)) {
		const saved = context?.config.input_defaults?.[key];
		const value = typeof saved === "string" ? saved : appDefault;
		if (typeof value === "string" && value.length <= 512) out.values[key] = value;
	}
	return out;
}
/** The app's own publish_settings grammars (cadence-app-social-content
*  screen-v2.mjs): a field that fails one is omitted, never coerced. */
var PUBLISH_DEST = /^[A-Za-z0-9._-]{1,120}$/;
var PUBLISH_ZONE = /^[A-Za-z0-9/_+-]+$/;
var CONTROL_CHARS = /[\u0000-\u001f\u007f-\u009f]/;
/** CAD-1902: the configured publication binding for this install and scope
*  at the installed bundle, as the frame's read-only publish_settings. The
*  mounted context's own binding wins over the install-wide one. */
function publishSettingsOf(installation, contextId, extras) {
	const candidates = (extras?.bindings ?? []).filter((b) => b.install_id === installation.install_id && b.state === "configured" && b.config.bundle_digest === installation.digest && b.config.publish && typeof b.config.publish === "object" && ((b.context_id ?? "") === contextId || (b.context_id ?? "") === ""));
	const binding = candidates.find((b) => (b.context_id ?? "") === contextId) ?? candidates.find((b) => (b.context_id ?? "") === "");
	if (!binding || !Number.isSafeInteger(binding.revision) || binding.revision < 0) return void 0;
	const publish = binding.config.publish;
	const out = { revision: binding.revision };
	if (typeof publish.destination_id === "string" && PUBLISH_DEST.test(publish.destination_id)) out.destination_id = publish.destination_id;
	if (typeof publish.destination_label === "string" && publish.destination_label.trim().length > 0 && publish.destination_label.length <= 80 && !CONTROL_CHARS.test(publish.destination_label)) out.destination_label = publish.destination_label;
	if (publish.toolkit === "instagram" || publish.toolkit === "facebook") out.toolkit = publish.toolkit;
	if (typeof publish.timezone === "string" && PUBLISH_ZONE.test(publish.timezone) && isZone(publish.timezone)) out.timezone = publish.timezone;
	return out;
}
function readinessOf(installation, contextId, extras) {
	const blockers = [];
	if (!installation.approved) blockers.push("install_unapproved");
	for (const slot of Object.keys(installation.capabilities ?? {})) if (!extras?.bindings.some((b) => b.slot === slot && (b.context_id ?? "") === contextId && b.state === "configured" && b.config.bundle_digest === installation.digest) && SLOT.test(slot) && slot.length <= 50) blockers.push(`slot_unbound.${slot}`);
	if (extras?.workers === 0) blockers.push("no_workers");
	const kept = blockers.slice(0, KEYS_MAX);
	return {
		ok: kept.length === 0,
		blockers: kept
	};
}
function screenProjection(installation, tag, contextId, contexts, runs, effects, intents, extras) {
	const active = contexts.filter((c) => c.install_id === installation.install_id && c.state === "active");
	if (contextId && !active.some((c) => c.id === contextId)) throw new Error("The screen context is unavailable");
	const scopedRuns = runs.filter((r) => r.install_id === installation.install_id && (!contextId || r.context_id === contextId));
	const scopedIds = new Set(scopedRuns.map((r) => r.id));
	const scopedEffects = effects.filter((e) => e.authority.install_id === installation.install_id && scopedIds.has(e.authority.run_id) && (!contextId || e.authority.context?.id === contextId));
	if (active.length > 128 || scopedRuns.length > 256 || scopedEffects.length > 256) throw new Error("The screen projection exceeds its read-only limit");
	const scoped = extras && extras.installId === installation.install_id && extras.contextId === contextId ? extras : void 0;
	const projection = {
		v: 1,
		op: "screen",
		install_id: installation.install_id,
		digest: installation.digest,
		tag,
		context_id: contextId,
		installation: {
			install_id: installation.install_id,
			name: text(installation.name, 128),
			title: text(installation.title),
			version: text(installation.version, 128),
			digest: installation.digest,
			summary: text(installation.summary, 4096)
		},
		contexts: active.map((c) => ({
			id: c.id,
			label: text(c.config.label)
		})),
		runs: scopedRuns.map((r) => {
			const v2 = runV2(r, installation, scoped);
			return {
				id: r.id,
				state: text(r.state, 64),
				title: text(r.snapshot.workflow.title),
				snapshot_digest: r.snapshot_digest,
				workflow: {
					title: text(r.snapshot.workflow.title),
					...v2.workflow
				},
				context_id: r.context_id ?? "",
				...v2.fields
			};
		}),
		outbox: scopedEffects.map((e) => ({
			effect_id: e.effect_id,
			state: text(e.state, 64),
			title: text(e.record.title ?? ""),
			run_id: e.authority.run_id,
			context_id: e.authority.context?.id ?? ""
		})),
		publish_intents: scopedIntents(intents, installation.install_id, contextId, new Map(scopedRuns.map((r) => [r.id, r.context_id ?? ""])), true),
		...present({ sources: sourcesOf(installation, scopedRuns, scoped) }),
		defaults: defaultsOf(installation, active.find((c) => c.id === contextId)),
		...present({ publish_settings: publishSettingsOf(installation, contextId, scoped) }),
		readiness: readinessOf(installation, contextId, scoped),
		actions: scoped ? [...ACTION_VERBS] : [],
		now: Math.floor(Date.now() / 1e3)
	};
	if (!shapeFor(projection, false)) throw new Error("The screen projection exceeds its message limit");
	return projection;
}
//#endregion
//#region scripts/app-fixtures/producer.ts
var MAX_INPUT = 1048576;
var MAX_OUTPUT = 8388608;
var CONTRACT = "app-screen-projection-fixtures/7";
var NOW = 1799999e3;
var SCENARIOS = [
	"empty",
	"loading",
	"unavailable",
	"ready",
	"draft",
	"review"
];
function exactKeys(value, keys, label) {
	if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).sort().join("\0") !== [...keys].sort().join("\0")) throw new Error(`invalid closed ${label} metadata`);
}
function runFor(installation, id, mode) {
	const contextId = "fixture-context";
	const state = mode !== "ready" ? "running" : "succeeded";
	const stepKind = mode === "review" ? "review_text" : "produce_text";
	return {
		id,
		install_id: installation.install_id,
		context_id: contextId,
		state,
		snapshot_digest: installation.digest,
		approved_digest: null,
		snapshot: {
			workflow: {
				title: mode === "ready" ? "Synthetic completed run" : "Synthetic draft",
				steps: [{
					id: "fixture-step",
					kind: stepKind,
					assignee: "fixture",
					dependencies: [],
					instruction: "Synthetic fixture only"
				}]
			},
			inputs: {},
			owner_pm: "",
			assignments: {},
			capabilities: {},
			context: {
				id: contextId,
				revision: 1,
				digest: installation.digest
			},
			publication: {
				slot: "",
				binding: null
			}
		},
		steps: [{
			step_id: "fixture-step",
			task_id: `task-${id}`,
			state: mode === "ready" ? "succeeded" : mode === "review" ? "dispatched" : "pending",
			message_id: null
		}],
		created: 1799998940,
		updated: NOW,
		artifacts: [],
		reviews: []
	};
}
function makeInstallation(input) {
	const capabilities = Object.fromEntries(Object.entries(input.manifest.capabilities).map(([name, declaration]) => [name, { ...declaration }]));
	return {
		install_id: "fixture-install",
		name: input.manifest.app,
		title: input.manifest.title,
		version: input.manifest.version,
		summary: input.manifest.summary ?? "",
		digest: input.bundle_digest,
		catalog_generation: "fixture-catalog",
		storage_kind: "workspace",
		project_link: null,
		approved: false,
		executable: true,
		approval: { state: "synthetic" },
		guide: "",
		files: [...input.inventory],
		capabilities,
		connection_slots: [],
		workflows: []
	};
}
function makeContext(installation) {
	return {
		id: "fixture-context",
		install_id: installation.install_id,
		revision: 1,
		digest: installation.digest,
		state: "active",
		config: {
			schema: 1,
			label: "Synthetic context",
			input_defaults: {}
		}
	};
}
function makeExtras(installation, contextId) {
	return {
		installId: installation.install_id,
		contextId,
		bindings: [],
		workers: null,
		texts: /* @__PURE__ */ new Map()
	};
}
function makeScenario(input, installation, context, label) {
	const contextId = label === "empty" ? "" : context.id;
	const contexts = label === "empty" ? [] : [context];
	const runs = label === "ready" || label === "draft" || label === "review" ? [runFor(installation, `fixture-${label}`, label)] : [];
	const effects = [];
	const extras = makeExtras(installation, contextId);
	const intents = label === "loading" ? void 0 : {
		installId: installation.install_id,
		contextId,
		status: label === "unavailable" ? "unavailable" : "ok",
		intents: []
	};
	const push = shapeFor(screenProjection(installation, input.screen.tag, contextId, contexts, runs, effects, intents, extras), "v2");
	if (!push) throw new Error(`canonical v2 shape refused fixture '${label}'`);
	return {
		label,
		push
	};
}
function validate(input) {
	exactKeys(input, [
		"contract",
		"bundle_digest",
		"manifest",
		"inventory",
		"screen",
		"kit"
	], "producer input");
	const value = input;
	if (value.contract !== CONTRACT || typeof value.bundle_digest !== "string" || !/^sha256:[0-9a-f]{64}$/.test(value.bundle_digest) || typeof value.kit !== "string" || !Array.isArray(value.inventory) || value.inventory.some((path) => typeof path !== "string") || typeof value.manifest.app !== "string" || typeof value.manifest.title !== "string" || typeof value.manifest.version !== "string" || !(value.manifest.summary === null || typeof value.manifest.summary === "string") || !value.manifest.capabilities || typeof value.manifest.capabilities !== "object" || Array.isArray(value.manifest.capabilities)) throw new Error("invalid fixture metadata");
	exactKeys(value.manifest, [
		"app",
		"title",
		"version",
		"summary",
		"capabilities"
	], "manifest");
	exactKeys(value.screen, [
		"tag",
		"contract",
		"may",
		"tools"
	], "screen");
	if (value.screen.contract !== "app-screens/v2" || typeof value.screen.tag !== "string" || !Array.isArray(value.screen.may) || !value.screen.tools || typeof value.screen.tools !== "object" || Array.isArray(value.screen.tools)) throw new Error("unsupported or mismatched screen contract");
	for (const declaration of Object.values(value.manifest.capabilities)) exactKeys(declaration, [
		"schema",
		"capability",
		"version",
		"action",
		"resource_kind",
		"effect"
	], "capability slot");
}
async function main() {
	const chunks = [];
	let inputBytes = 0;
	for await (const chunk of process.stdin) {
		inputBytes += chunk.byteLength;
		if (inputBytes > MAX_INPUT) throw new Error("fixture input exceeds bound");
		chunks.push(chunk);
	}
	const bytes = new Uint8Array(inputBytes);
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	const input = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
	validate(input);
	const oldNow = Date.now;
	Date.now = () => NOW * 1e3;
	try {
		const installation = makeInstallation(input);
		const context = makeContext(installation);
		const scenarios = SCENARIOS.map((label) => makeScenario(input, installation, context, label));
		const result = {
			fixture: true,
			contract: CONTRACT,
			kit: input.kit,
			screen_contract: input.screen.contract,
			bundle_digest: input.bundle_digest,
			screen: {
				tag: input.screen.tag,
				app: input.manifest.app,
				may: input.screen.may,
				tools: input.screen.tools
			},
			scenarios
		};
		const output = JSON.stringify(result);
		if (new TextEncoder().encode(output).byteLength > MAX_OUTPUT) throw new Error("fixture output exceeds bound");
		process.stdout.write(output);
	} finally {
		Date.now = oldNow;
	}
}
main().catch((error) => {
	process.stderr.write(`fixture producer refused: ${String(error?.message ?? "error").slice(0, 512)}\n`);
	process.exitCode = 1;
});
//#endregion

//# sourceMappingURL=producer.mjs.map