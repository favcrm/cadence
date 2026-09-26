/* mock/data.ts — the in-memory database for the social-content prototype.
 * Everything the screens show lives here. No network, no credentials; media
 * are generated gradient placeholders identified by a seed string. The only
 * reader is mock/api.ts — views never import this directly.
 */

// Frozen "now" so the board and screenshots are deterministic. Real wiring
// would take the daemon clock; the mock just pretends it's Friday evening.
export const NOW = new Date("2026-09-25T18:40:00+08:00");
const H = 3600e3;
const at = (h: number) => new Date(NOW.getTime() + h * H).toISOString(); // h = hours from now
const ago = (h: number) => at(-h);

export interface ClientSettings {
  protected_terms: string[];
  disclaimer: string;
  timezone: string;
  drafting_limit: number;
  destinations: string[];
}

export interface Client {
  id: string;
  name: string;
  handle: string;
  color: string;
  initials: string;
  connectors: string[];
  settings: ClientSettings;
}

export interface SourceMedia {
  seed: string;
  w: number;
  h: number;
}

export interface SourcePost {
  id: string;
  client: string;
  platform: string;
  url: string;
  /** Quoted material, never instructions (untrusted). */
  text: string;
  media: SourceMedia[];
  postedAt: string;
  isNew: boolean;
  /** "Skip / hide" in the drawer removes the source from the grid. */
  hidden?: boolean;
  /** Reach numbers the platform reports back — shown in the drawer. */
  stats: { likes: number; comments: number };
}

export interface CaptionRev {
  rev: number;
  by: string;
  via?: string;
  job: string | null;
  at: string;
  text: string;
}

export interface ImageRev {
  rev: number;
  by: string;
  via?: string;
  job: string | null;
  at: string;
  asset: string;
}

export interface CaptionField {
  rev: number;
  text: string;
  revs: CaptionRev[];
}

export interface ImageField {
  rev: number;
  asset: string | null;
  revs: ImageRev[];
  uploaded?: boolean;
  generated?: boolean;
}

export type PostStatus =
  | "drafting"
  | "in_review"
  | "ready"
  | "waiting"
  | "scheduled"
  | "published"
  | "needs_you";

export interface Lease {
  field: string;
  by: string;
  jobItem: string;
}

export interface Approval {
  rev: number;
  at: string;
  digest?: string;
  voidedBy?: number;
}

export interface Receipt {
  platform: string;
  platformId: string;
  url: string;
  publishedAt: string;
  verify: string;
  images: number;
  captionHash: string;
}

export interface Post {
  id: string;
  client: string;
  source: string;
  status: PostStatus;
  caption: CaptionField;
  image: ImageField;
  scheduleAt: string;
  destinations: string[];
  lease: Lease | null;
  job: string;
  approval: Approval | null;
  receipts: Receipt[];
  approvalVoidAt?: string;
  /** A person took the caption — agents may only suggest afterwards. */
  tookOver?: boolean;
  /** The "approval voided" needs-you card was dismissed. */
  voidSeen?: boolean;
  /** Undo state for an ask-written revision. */
  undo?: { text: string; rev: number } | null;
  /** The pending writer diff to show after an ask. */
  diff?: { from: string; to: string; rev: number } | null;
}

export type StepState = "waiting" | "running" | "done" | "failed" | "mismatch" | "skipped";

export interface RunItem {
  post: string;
  steps: Record<string, StepState>;
}

export interface Run {
  id: string;
  kind: string;
  client: string;
  status: "running" | "done";
  at: string;
  label: string;
  items: RunItem[];
  log: [string, string][];
}

export interface Suggestion {
  id: string;
  post: string;
  field: "caption" | "image";
  by: string;
  at: string;
  note: string;
  text: string;
  done?: boolean;
  rejected?: boolean;
}

export interface DigestItem {
  post: string;
  pinnedRev: number;
  hold: boolean;
  voided?: number;
}

export interface Digest {
  id: string;
  run: string;
  client: string;
  at: string;
  status: "pending" | "approved";
  items: DigestItem[];
}

export interface Automation {
  id: string;
  name: string;
  desc: string;
  every: string;
  on: boolean;
  last: string;
  approval: string;
}

export interface Workflow {
  id: string;
  title: string;
  file: string;
  steps: string[];
  note: string;
}

export interface MockDb {
  clients: Client[];
  sourcePosts: SourcePost[];
  posts: Post[];
  runs: Run[];
  suggestions: Suggestion[];
  digests: Digest[];
  automations: Automation[];
  workflows: Workflow[];
  /** Library multi-select lives here (UI state of the mock). */
  selection: Set<string>;
  activity: [string, string][];
  currentClient: string;
  /** Minutes the pretend clock has advanced since NOW. */
  clock: number;
  nextRun: number;
  nextPost: number;
}

const clients: Client[] = [
  {
    id: "kura", name: "Kura Ramen 蔵", handle: "kura.ramen.hk",
    color: "#e05d44", initials: "蔵",
    connectors: ["instagram", "facebook"],
    settings: {
      protected_terms: ["Kura Ramen", "蔵", "HK$128", "#KuraHK"],
      disclaimer: "All prices in HKD",
      timezone: "Asia/Hong_Kong",
      drafting_limit: 20,
      destinations: ["instagram", "facebook"],
    },
  },
  {
    id: "velvet", name: "Velvet Padel Club", handle: "velvet.padel",
    color: "#7c5cd6", initials: "V",
    connectors: ["instagram", "web"],
    settings: {
      protected_terms: ["Velvet Padel", "HK$380", "#VelvetPadel"],
      disclaimer: "",
      timezone: "Asia/Hong_Kong",
      drafting_limit: 10,
      destinations: ["instagram"],
    },
  },
];

// Source posts — written by the feed, read-only, text is untrusted.
const sourcePosts: SourcePost[] = [
  { id: "s1", client: "kura", platform: "instagram", url: "https://instagram.com/p/CxKura01",
    text: "NEW: Black garlic tonkotsu — 18-hour broth, limited to 40 bowls a day. HK$128. From Thursday at both shops. #KuraHK",
    media: [{ seed: "ramen-black", w: 1080, h: 1080 }, { seed: "broth-pour", w: 1080, h: 1080 }, { seed: "bowl-side", w: 1080, h: 1350 }],
    postedAt: ago(9), isNew: false, stats: { likes: 412, comments: 37 } },
  { id: "s2", client: "kura", platform: "facebook", url: "https://facebook.com/kura.ramen.hk/posts/8801",
    text: "Our Central shop closes early on Oct 1 (last order 21:30) for a private event. TST stays open till 23:00 as usual.",
    media: [{ seed: "shop-front", w: 1200, h: 900 }], postedAt: ago(14), isNew: false,
    stats: { likes: 88, comments: 12 } },
  { id: "s3", client: "kura", platform: "instagram", url: "https://instagram.com/p/CxKura07",
    text: "Meet the chef: Mori-san on why we flame the chashu to order. Full video on the site →",
    media: [{ seed: "chef-flame", w: 1080, h: 1080 }, { seed: "chef-hands", w: 1080, h: 1080 }],
    postedAt: ago(30), isNew: true, stats: { likes: 265, comments: 19 } },
  { id: "s4", client: "kura", platform: "instagram", url: "https://instagram.com/p/CxKura12",
    text: "Thanks @hkfoodlover for the feature — “the richest broth in Central” 🍜",
    media: [{ seed: "bowl-top", w: 1080, h: 1350 }], postedAt: ago(52), isNew: false,
    stats: { likes: 531, comments: 44 } },
  { id: "s5", client: "kura", platform: "facebook", url: "https://facebook.com/kura.ramen.hk/posts/8790",
    text: "Autumn collab: Kura x Yardley Bros — a yuzu wheat lager, pours at both shops from Friday.",
    media: [{ seed: "beer-pour", w: 1200, h: 900 }], postedAt: ago(70), isNew: false,
    stats: { likes: 143, comments: 8 } },
  { id: "s6", client: "kura", platform: "web", url: "https://kura.hk/journal/queue-times",
    text: "Weekend queue update: average wait 12 min at TST, 20 min at Central. Book ahead in the app.",
    media: [], postedAt: ago(96), isNew: false, stats: { likes: 0, comments: 0 } },

  { id: "s7", client: "velvet", platform: "instagram", url: "https://instagram.com/p/CxVel01",
    text: "Courts open 07:00–23:00 daily from October. Bookings live on the app. #VelvetPadel",
    media: [{ seed: "court-aerial", w: 1080, h: 1080 }], postedAt: ago(6), isNew: false,
    stats: { likes: 198, comments: 15 } },
  { id: "s8", client: "velvet", platform: "instagram", url: "https://instagram.com/p/CxVel04",
    text: "Autumn ladder: 64 players, 3 weeks, finals Oct 18. Spectators welcome — the bar stays open.",
    media: [{ seed: "ladder-night", w: 1080, h: 1080 }], postedAt: ago(20), isNew: true,
    stats: { likes: 156, comments: 21 } },
  { id: "s9", client: "velvet", platform: "instagram", url: "https://instagram.com/p/CxVel09",
    text: "New coach alert — @marta.padel joins Velvet from Oct 1. Clinics every Tue/Thu.",
    media: [{ seed: "coach-marta", w: 1080, h: 1350 }], postedAt: ago(33), isNew: false,
    stats: { likes: 322, comments: 41 } },
  { id: "s10", client: "velvet", platform: "web", url: "https://velvetpadel.hk/membership",
    text: "Memberships from HK$380/month — off-peak, all courts, guest passes included.",
    media: [{ seed: "membership", w: 1200, h: 630 }], postedAt: ago(80), isNew: false,
    stats: { likes: 0, comments: 0 } },
  { id: "s11", client: "velvet", platform: "instagram", url: "https://instagram.com/p/CxVel15",
    text: "Recovery zone now open: sauna + ice bath, members first.",
    media: [{ seed: "sauna", w: 1080, h: 1080 }], postedAt: ago(120), isNew: false,
    stats: { likes: 274, comments: 18 } },
  { id: "s12", client: "velvet", platform: "instagram", url: "https://instagram.com/p/CxVel21",
    text: "Pro shop drop: Velvet x Siux rackets, 20 units, members 48h early access.",
    media: [{ seed: "racket-drop", w: 1080, h: 1350 }, { seed: "racket-box", w: 1080, h: 1350 }],
    postedAt: ago(150), isNew: true, stats: { likes: 189, comments: 26 } },
  { id: "s13", client: "kura", platform: "instagram", url: "https://instagram.com/p/CxKura19",
    text: "TSUKEMEN returns for October — thicker noodles, concentrated dip, TST shop only.",
    media: [{ seed: "tsukemen", w: 1080, h: 1080 }], postedAt: ago(40), isNew: true,
    stats: { likes: 377, comments: 29 } },
];

/* Posts (drafts). Revisions are append-only per field; approvals pin a
 * revision hash; receipts are system-written. Statuses follow app.md:
 * drafting → in_review → ready → waiting → scheduled → published, with
 * needs_you on a verify mismatch.
 */
const posts: Post[] = [
  {
    id: "p1", client: "kura", source: "s1", status: "drafting",
    caption: { rev: 1, text: "黑蒜豚骨——18 小時熬湯,每日限量 40 碗。HK$128…",
      revs: [{ rev: 1, by: "writer", job: "run-103", at: ago(0.35),
               text: "黑蒜豚骨——18 小時熬湯,每日限量 40 碗。HK$128…" }] },
    image: { rev: 0, asset: null, revs: [] },
    scheduleAt: at(68), destinations: ["instagram", "facebook"],
    lease: { field: "caption", by: "writer", jobItem: "run-103:i1" },
    job: "run-103", approval: null, receipts: [],
  },
  {
    id: "p2", client: "kura", source: "s2", status: "in_review",
    caption: { rev: 3,
      text: "10 月 1 日中環店私人活動,提早至 21:30 截單。尖沙咀店照常營業至 23:00。All prices in HKD",
      revs: [
        { rev: 1, by: "writer", job: "run-102", at: ago(9),
          text: "中環店 10 月 1 日有私人活動,最後點餐 21:30。尖沙咀照常到 23:00。" },
        { rev: 2, by: "you", job: null, at: ago(7.5),
          text: "10 月 1 日中環店私人活動,提早截單 21:30;尖沙咀照常至 23:00。" },
        { rev: 3, by: "writer", via: "asked by you", job: "run-104", at: ago(3),
          text: "10 月 1 日中環店私人活動,提早至 21:30 截單。尖沙咀店照常營業至 23:00。All prices in HKD" },
      ] },
    image: { rev: 1, asset: "shop-front", revs: [{ rev: 1, by: "designer", job: "run-102", at: ago(8.8), asset: "shop-front" }] },
    scheduleAt: at(30), destinations: ["instagram", "facebook"],
    lease: null, job: "run-102",
    // Was approved at r2 in a previous digest; the r3 edit voided it.
    approval: { rev: 2, at: ago(6), voidedBy: 3 },
    receipts: [],
  },
  {
    id: "p3", client: "kura", source: "s5", status: "waiting",
    caption: { rev: 2,
      text: "Kura x Yardley Bros 秋季聯乘:柚子小麥啤酒,星期五起兩店供應。#KuraHK",
      revs: [
        { rev: 1, by: "writer", job: "run-102", at: ago(9.2),
          text: "秋季限定:Kura 與 Yardley Bros 推出柚子小麥啤酒,星期五兩店開賣。#KuraHK" },
        { rev: 2, by: "editor", job: "run-102", at: ago(8.1),
          text: "Kura x Yardley Bros 秋季聯乘:柚子小麥啤酒,星期五起兩店供應。#KuraHK" },
      ] },
    image: { rev: 1, asset: "beer-pour", revs: [{ rev: 1, by: "designer", job: "run-102", at: ago(8.9), asset: "beer-pour" }] },
    scheduleAt: at(39), destinations: ["instagram", "facebook"],
    lease: null, job: "run-102",
    approval: { rev: 2, at: ago(1.2), digest: "d1" },
    receipts: [],
  },
  {
    id: "p4", client: "velvet", source: "s9", status: "in_review",
    caption: { rev: 2,
      text: "新教練加入!Marta 由 10 月 1 日起加入 Velvet Padel,每週二、四開設訓練班。名額有限,會員優先預約。#VelvetPadel",
      revs: [
        { rev: 1, by: "writer", job: "run-106", at: ago(9.4),
          text: "好消息!新教練 Marta 將於 10 月 1 日正式加入 Velvet Padel,逢星期二及星期四開班授課,名額有限,會員可優先預約。#VelvetPadel" },
        { rev: 2, by: "you", job: null, at: ago(5),
          text: "新教練加入!Marta 由 10 月 1 日起加入 Velvet Padel,每週二、四開設訓練班。名額有限,會員優先預約。#VelvetPadel" },
      ] },
    image: { rev: 1, asset: "coach-marta", revs: [{ rev: 1, by: "designer", job: "run-106", at: ago(9.1), asset: "coach-marta" }] },
    scheduleAt: at(63), destinations: ["instagram"],
    lease: null, job: "run-106", approval: null, receipts: [],
  },
  {
    id: "p5", client: "velvet", source: "s11", status: "scheduled",
    caption: { rev: 2,
      text: "Recovery Zone 正式開放:桑拿 + 冰浴,會員優先使用。#VelvetPadel",
      revs: [
        { rev: 1, by: "writer", job: "run-100", at: ago(25),
          text: "全新 Recovery Zone 開放:桑拿及冰浴設施,會員優先體驗。#VelvetPadel" },
        { rev: 2, by: "editor", job: "run-100", at: ago(24.4),
          text: "Recovery Zone 正式開放:桑拿 + 冰浴,會員優先使用。#VelvetPadel" },
      ] },
    image: { rev: 1, asset: "sauna", revs: [{ rev: 1, by: "designer", job: "run-100", at: ago(24.8), asset: "sauna" }] },
    scheduleAt: at(2.6), destinations: ["instagram"],
    lease: null, job: "run-100", approval: { rev: 2, at: ago(20), digest: "d9" },
    receipts: [],
  },
  {
    id: "p6", client: "kura", source: "s4", status: "published",
    caption: { rev: 3,
      text: "多謝 @hkfoodlover 推介——「中環最濃郁湯底」🍜 #KuraHK",
      revs: [
        { rev: 1, by: "writer", job: "run-101", at: ago(26),
          text: "感謝 @hkfoodlover 的報導,稱我們是「中環最濃郁湯底」🍜 #KuraHK" },
        { rev: 2, by: "you", job: null, at: ago(24),
          text: "多謝 @hkfoodlover 推介——「中環最濃郁湯底」🍜 #KuraHK" },
        { rev: 3, by: "editor", job: "run-101", at: ago(23),
          text: "多謝 @hkfoodlover 推介——「中環最濃郁湯底」🍜 #KuraHK" },
      ] },
    image: { rev: 1, asset: "bowl-top", revs: [{ rev: 1, by: "designer", job: "run-101", at: ago(25), asset: "bowl-top" }] },
    scheduleAt: ago(21), destinations: ["instagram", "facebook"],
    lease: null, job: "run-101", approval: { rev: 3, at: ago(22), digest: "d0" },
    receipts: [
      { platform: "instagram", platformId: "1795542100", url: "https://instagram.com/p/CxKura99",
        publishedAt: ago(21), verify: "ok", images: 1, captionHash: "8f2c" },
      { platform: "facebook", platformId: "8807", url: "https://facebook.com/kura.ramen.hk/posts/8807",
        publishedAt: ago(21), verify: "ok", images: 1, captionHash: "8f2c" },
    ],
  },
  {
    id: "p7", client: "kura", source: "s6", status: "needs_you",
    caption: { rev: 3,
      text: "週末等位時間:尖沙咀平均 12 分鐘,中環 20 分鐘。App 內可提前訂位。#KuraHK",
      revs: [
        { rev: 1, by: "writer", job: "run-101", at: ago(25.5), text: "週末等位:尖沙咀 12 分鐘,中環 20 分鐘。可用 App 訂位。#KuraHK" },
        { rev: 2, by: "editor", job: "run-101", at: ago(25.1), text: "週末等位時間:尖沙咀平均 12 分鐘,中環 20 分鐘。App 內可提前訂位。#KuraHK" },
        { rev: 3, by: "you", job: null, at: ago(24.6), text: "週末等位時間:尖沙咀平均 12 分鐘,中環 20 分鐘。App 內可提前訂位。#KuraHK" },
      ] },
    image: { rev: 1, asset: "queue-2up", revs: [{ rev: 1, by: "designer", job: "run-101", at: ago(25.2), asset: "queue-2up" }] },
    scheduleAt: ago(19), destinations: ["instagram"],
    lease: null, job: "run-101", approval: { rev: 3, at: ago(20), digest: "d0" },
    receipts: [
      // The verify mismatch: approved r3 had a 2-image set and hash 31aa;
      // what went out was a single image and a stale caption hash 8f2c.
      { platform: "instagram", platformId: "1795542118", url: "https://instagram.com/p/CxKur104",
        publishedAt: ago(19), verify: "mismatch", images: 1, captionHash: "8f2c" },
    ],
  },
  {
    id: "p8", client: "velvet", source: "s7", status: "drafting",
    caption: { rev: 0, text: "", revs: [] },
    image: { rev: 0, asset: null, revs: [] },
    scheduleAt: at(75), destinations: ["instagram"],
    lease: null, job: "run-103", approval: null, receipts: [],
  },
  {
    id: "p9", client: "velvet", source: "s10", status: "waiting",
    caption: { rev: 1,
      text: "會籍由 HK$380/月起:非繁忙時段任用所有球場,附送訪客券。#VelvetPadel",
      revs: [
        { rev: 1, by: "writer", job: "run-106", at: ago(9.6),
          text: "會籍由 HK$380/月起:非繁忙時段任用所有球場,附送訪客券。#VelvetPadel" },
      ] },
    image: { rev: 1, asset: "membership", revs: [{ rev: 1, by: "designer", job: "run-106", at: ago(9.3), asset: "membership" }] },
    scheduleAt: at(87), destinations: ["instagram"],
    lease: null, job: "run-106",
    approval: { rev: 1, at: ago(1.1), digest: "d2" },
    receipts: [],
  },
];

/* Jobs = workflow runs over selected records; each item carries per-step
 * state. run-103 is seeded mid-flight and advances on the mock clock. */
const runs: Run[] = [
  {
    id: "run-100", kind: "social-localize", client: "velvet", status: "done",
    at: ago(25.6), label: "Draft 1 post",
    items: [
      { post: "p5", steps: { adapt: "done", visuals: "done", review: "done", schedule: "done", verify: "waiting" } },
    ],
    log: [
      [ago(25.6), "run created from Library selection (1 source)"],
      [ago(20), "digest d9 approved (1 post)"],
    ],
  },
  {
    id: "run-101", kind: "social-localize", client: "kura", status: "done",
    at: ago(26), label: "Draft 2 posts",
    items: [
      { post: "p6", steps: { adapt: "done", visuals: "done", review: "done", schedule: "done", verify: "done" } },
      { post: "p7", steps: { adapt: "done", visuals: "done", review: "done", schedule: "done", verify: "mismatch" } },
    ],
    log: [
      [ago(26), "run created from Library selection (2 sources)"],
      [ago(24.6), "p7 caption edited by you → r3"],
      [ago(20), "digest d0 approved (2 posts)"],
      [ago(19), "verify p7: receipt image count 1 ≠ approved 2 → needs-you"],
    ],
  },
  {
    id: "run-102", kind: "social-localize", client: "kura", status: "done",
    at: ago(9.6), label: "Draft 2 posts",
    items: [
      { post: "p2", steps: { adapt: "done", visuals: "done", review: "done", schedule: "done", verify: "waiting" } },
      { post: "p3", steps: { adapt: "done", visuals: "done", review: "done", schedule: "done", verify: "waiting" } },
    ],
    log: [
      [ago(9.6), "run created from Library selection (2 sources)"],
      [ago(8.1), "p3 review passed after editor touch-up → r2"],
      [ago(1.2), "digest d1 staged — waiting for approval"],
    ],
  },
  {
    id: "run-106", kind: "social-localize", client: "velvet", status: "done",
    at: ago(9.7), label: "Draft 2 posts",
    items: [
      { post: "p4", steps: { adapt: "done", visuals: "done", review: "done", schedule: "waiting", verify: "waiting" } },
      { post: "p9", steps: { adapt: "done", visuals: "done", review: "done", schedule: "done", verify: "waiting" } },
    ],
    log: [
      [ago(9.7), "run created from Library selection (2 sources)"],
      [ago(9.3), "p4 caption touched by you — left to you"],
      [ago(1.1), "digest d2 staged — waiting for approval"],
    ],
  },
  {
    id: "run-103", kind: "social-localize", client: "kura", status: "running",
    at: ago(0.4), label: "Draft 1 post",
    items: [
      { post: "p1", steps: { adapt: "running", visuals: "waiting", review: "waiting", schedule: "waiting", verify: "waiting" } },
    ],
    log: [
      [ago(0.4), "run created from Library selection (1 source)"],
      [ago(0.35), "writer holds the caption lease on p1"],
    ],
  },
  {
    id: "run-105", kind: "social-localize", client: "velvet", status: "running",
    at: ago(0.3), label: "Draft 1 post",
    items: [
      { post: "p8", steps: { adapt: "running", visuals: "waiting", review: "waiting", schedule: "waiting", verify: "waiting" } },
    ],
    log: [[ago(0.3), "run created from Library selection (1 source)"]],
  },
  {
    id: "run-104", kind: "revise", client: "kura", status: "done",
    at: ago(3.1), label: "Ask: “add the prices line back” on p2",
    items: [{ post: "p2", steps: { change: "done" } }],
    log: [[ago(3.1), "ask by you → revise p2 (scope: caption)"], [ago(3.0), "writer wrote r3, diff shown to you"]],
  },
];

// Pending proposals on human-owned fields (agents may only suggest there).
const suggestions: Suggestion[] = [
  {
    id: "sg1", post: "p4", field: "caption", by: "editor", at: ago(2.2),
    note: "Tighter for the feed — keeps the coach tag, drops the second clause.",
    text: "新教練 Marta 加入 Velvet Padel!10 月 1 日起,逢週二四開班。#VelvetPadel",
  },
  {
    id: "sg2", post: "p2", field: "caption", by: "editor", at: ago(1.5),
    note: "Adds the prices-in-HKD line your settings ask for.",
    text: "10 月 1 日中環店私人活動,提早至 21:30 截單。尖沙咀店照常營業至 23:00。#KuraHK All prices in HKD",
  },
];

// One staged send digest per client; items pin (post, revision).
const digests: Digest[] = [
  {
    id: "d1", run: "run-102", client: "kura", at: ago(1.2), status: "pending",
    items: [
      { post: "p3", pinnedRev: 2, hold: false },
    ],
  },
  {
    id: "d2", run: "run-106", client: "velvet", at: ago(1.1), status: "pending",
    items: [
      { post: "p9", pinnedRev: 1, hold: false },
    ],
  },
];

const automations: Automation[] = [
  { id: "a1", name: "Scan sources", desc: "Pull new posts from connected sources; they land in Library marked new.",
    every: "every 6h", on: true, last: "3 new posts, 2h ago", approval: "standing: scan" },
  { id: "a2", name: "Evening draft pass", desc: "Draft from new sources, up to the daily limit.", every: "Mon–Fri 18:00 HKT",
    on: false, last: "never run", approval: "standing: drafting (no sends, ≤ limit/day)" },
  { id: "a3", name: "Friday schedule digest", desc: "Collect the week's ready posts into one approve card.", every: "Fri 09:00 HKT",
    on: true, last: "produced digest d1", approval: "sends always wait — digest" },
  { id: "a4", name: "Verify receipts", desc: "Compare what went out with the approved revision after every publish.",
    every: "after each publish", on: true, last: "caught a mismatch on p7", approval: "system" },
];

const workflows: Workflow[] = [
  { id: "social-localize", title: "Localise and schedule social posts", file: "workflows/social-localize.md",
    steps: ["adapt → writer", "visuals → designer", "review → editor", "schedule → publisher", "verify → analyst"],
    note: "Runs over selected source posts; creates one post record per item." },
  { id: "revise", title: "Ask the agent — revise one post", file: "workflows/revise.md",
    steps: ["change → writer/designer"],
    note: "mode: direct — the person asked, so it writes a revision and shows diff + undo." },
  { id: "scan-sources", title: "Scan source feeds for new posts", file: "workflows/scan-sources.md",
    steps: ["pull → scout"],
    note: "Scheduled; writes unseen source_post records, text marked untrusted." },
  { id: "blog-post", title: "Write a blog post", file: "workflows/blog-post.md",
    steps: ["brief → strategist", "draft → writer", "images → designer", "review → editor"],
    note: "Starts outside the drafting standing approval — a plan card would open first." },
];

export const M: MockDb = {
  clients, sourcePosts, posts, runs, suggestions, digests, automations, workflows,
  selection: new Set<string>(),
  activity: [
    [ago(1.2), "digest d1 staged by publisher (run-102)"],
    [ago(1.5), "editor proposed a caption change on p2"],
    [ago(2.2), "editor proposed a caption change on p4"],
    [ago(3.0), "writer wrote p2 r3 (asked by you)"],
    [ago(19.0), "verify p7 failed — needs-you"],
  ],
  currentClient: "kura",
  clock: 0,
  nextRun: 107,
  nextPost: 10,
};
