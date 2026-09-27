// Distinct immutable source records and derived posts. All dates are local fixture plans.
export const fixtureTimeZone = "Asia/Hong_Kong";
const dateParts = new Intl.DateTimeFormat("en-CA", {
  timeZone: fixtureTimeZone,
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
}).formatToParts(new Date());
const part = (name) => dateParts.find((value) => value.type === name).value;
const monday = new Date(
  `${part("year")}-${part("month")}-${part("day")}T12:00:00Z`,
);
monday.setUTCDate(monday.getUTCDate() - ((monday.getUTCDay() + 6) % 7));
export const fixtureWeek = monday.toISOString().slice(0, 10);
export const sources = [
  {
    id: "source-1",
    brand: "Kura Ramen",
    platform: "instagram",
    text: "Black garlic tonkotsu — 18-hour broth, limited to 40 bowls a day. HK$128. #KuraHK",
    media: ["black garlic ramen", "broth preparation"],
    isNew: false,
    age: "9h ago",
  },
  {
    id: "source-2",
    brand: "Kura Ramen",
    platform: "facebook",
    text: "Our Central shop closes early on October 1. Last order 21:30. TST stays open until 23:00.",
    media: ["shop front"],
    isNew: false,
    age: "14h ago",
  },
  {
    id: "source-3",
    brand: "Kura Ramen",
    platform: "instagram",
    text: "Meet the chef: Mori-san on why we flame the chashu to order.",
    media: ["chef flame"],
    isNew: true,
    age: "1d ago",
  },
  {
    id: "source-4",
    brand: "Velvet Padel",
    platform: "instagram",
    text: "Courts open 07:00–23:00 daily from October. #VelvetPadel",
    media: ["court aerial"],
    isNew: true,
    age: "6h ago",
  },
  {
    id: "source-5",
    brand: "Velvet Padel",
    platform: "web",
    text: "Memberships from HK$380/month — off-peak courts and guest passes included.",
    media: ["membership"],
    isNew: false,
    age: "3d ago",
  },
  {
    id: "source-6",
    brand: "Kura Ramen",
    platform: "instagram",
    text: "TSUKEMEN returns for October — thicker noodles, concentrated dip. TST only.",
    media: ["tsukemen"],
    isNew: true,
    age: "2d ago",
  },
];
const states = [
  "draft",
  "review",
  "waiting",
  "scheduled",
  "published",
  "review",
];
export const studioFixture = {
  sources,
  posts: sources.map((source, index) => {
    const date = new Date(monday);
    date.setUTCDate(date.getUTCDate() + index);
    const at = `${date.toISOString().slice(0, 10)}T${index % 2 ? "16:30" : "09:30"}`;
    return {
      id: `fixture-${index + 1}`,
      sourceId: source.id,
      brand: source.brand,
      caption: source.text,
      media: [...source.media],
      revision: 1,
      status: states[index],
      scheduleAt: at,
      destinations: [source.platform === "web" ? "instagram" : source.platform],
      reviewedRevision: index >= 2 && index <= 4 ? 1 : null,
      approvalRevision: index >= 3 && index <= 4 ? 1 : null,
      needsYou:
        index === 5
          ? "Illustrative verification mismatch — no platform was contacted."
          : null,
      lease: index === 0,
      history: [
        {
          revision: 1,
          by: "fixture seed",
          caption: source.text,
          note: "Illustrative record, not an agent execution.",
        },
      ],
      outbox: null,
    };
  }),
  runs: [],
  suggestions: [
    {
      id: "suggestion-1",
      postId: "fixture-2",
      text: "Central: last order 21:30 on October 1. TST open until 23:00.",
      note: "Fixture suggestion: make the opening-hours update shorter.",
    },
  ],
};

// Illustrative busy Tuesday: distinct source-derived records, not generated posts or sends.
const tuesday = new Date(monday);
tuesday.setUTCDate(tuesday.getUTCDate() + 1);
for (const [index, source] of sources.slice(0, 4).entries()) {
  const base = structuredClone(studioFixture.posts[index]);
  studioFixture.posts.push({
    ...base,
    id: `fixture-${sources.length + index + 1}`,
    lease: false,
    scheduleAt: `${tuesday.toISOString().slice(0, 10)}T${["09:00", "11:00", "13:00", "18:45"][index]}`,
  });
}
