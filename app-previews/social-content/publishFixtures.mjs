// CAD-787 fixture data for the publish decision preview.
//
// Everything here is a hard-coded illustration: the exact pilot destination,
// one reviewed caption with its retained image, the separated cost quote and
// the full catalogue of dispatch states. No provider, credential, receipt or
// send exists. Product UI binds these slots to the CAD-771 store states
// (queued/cancelled/processing/posted/refused/held) and the exact 28-code
// refusal vocabulary — there is no pending, uncertain or held-reconnect
// state, and no preflight call.

export const publishDestination = {
  channel: "Instagram",
  handle: "@sakeboyhk",
  url: "https://www.instagram.com/sakeboyhk",
  accountKind: "Professional account",
  accountId: "ig-prof-fixture-0417",
  capability: "social.post → this exact account",
  scope: "instagram_basic + instagram_content_publish (fixture)",
  bindingRevision: "binding rev 12",
  grant: "Standing publish grant · owner-authorized · fixture window",
  statusNote: "Connected in fixture — discovery confirmed before any send.",
};

export const publishFrozen = {
  postId: "fixture-publish-1",
  sourceId: "source-3",
  revision: 3,
  caption:
    "Meet the chef: Mori-san on why we flame the chashu to order. Black garlic tonkotsu, 18-hour broth — 40 bowls a day. #KuraHK",
  media: ["chef flame (retained bytes)"],
  captionDigest: "sha256:9f2c…a41d (fixture)",
  mediaDigest: "sha256:77b0…c932 (fixture)",
  destinationDigest: "sha256:1e64…08bb (fixture)",
  idempotencyKey: "fixture illustration — not a secret key",
  writer: "writer (fixture)",
  reviewer: "editor (fixture)",
  reviewedAt: "fixture review · r3 pinned",
  scheduleDefault: "18:30",
};

export const publishCost = {
  generationQuote: "HK$12",
  generationState: "Quoted and approved separately — drafting only",
  publishCharge: "HK$0 fixture send — no provider charge illustrated",
  authorityNote:
    "Standing drafting approval does not authorise publishing. This publish decision needs its own human approval below.",
};

// Every dispatch state the product UI must render, each with the copy and the
// next actions the operator gets. `tone` maps to the approved badge classes.
// The uncertain entry is a reading tile (`badge: "reading"`), not a state.
export const publishStates = [
  {
    id: "queued",
    title: "Queued",
    tone: "b-info",
    copy: "Frozen intent stored with due time, timezone and idempotency key. Waiting for dispatch.",
    actions: ["Cancel before dispatch"],
  },
  {
    id: "processing",
    title: "Processing",
    tone: "b-info",
    copy: "Provider call in flight. Do not retry from here — reconcile first.",
    actions: ["Reconcile before any retry"],
  },
  {
    id: "posted",
    title: "Posted · verified",
    tone: "b-acc",
    copy: "Verified permalink and receipt bound to the approved revision. Only a receipt counts — a bare success string does not.",
    actions: ["Open permalink (fixture, disabled)", "Verify receipt"],
  },
  {
    id: "refused",
    title: "Refused",
    tone: "b-fail",
    copy: "Provider or gate refused the send (e.g. revoked grant, changed digest). Nothing was published.",
    actions: ["Inspect reason", "Return to approval"],
  },
  {
    id: "uncertain-reading",
    badge: "reading",
    title: "Uncertain reading",
    tone: "b-warn",
    copy: "No stored uncertain state: a processing intent with a lost response reads as uncertain — reconcile the upstream ledger before any retry, never duplicate. A recheck mismatch stores held instead, which needs a human.",
    actions: ["Reconcile upstream ledger", "Hold for a human decision"],
  },
  {
    id: "cancelled",
    title: "Cancelled",
    tone: "",
    copy: "Operator cancelled before dispatch. The frozen intent is kept for audit; nothing was sent.",
    actions: ["Schedule again (new approval)"],
  },
  {
    id: "held",
    title: "Held · needs human",
    tone: "b-warn",
    copy: "Authority expired or changed at dispatch (grant, binding, app or context). The intent is held for a new human decision — never silently published, never auto-resumed. Provider reconnect_needed is a separate layer.",
    actions: ["Reconnect account", "Re-approve after reconnect"],
  },
];
