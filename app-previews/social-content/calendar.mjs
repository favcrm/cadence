// Plain Hong Kong planning dates, not external timestamps or event durations.
export function postsForDay(posts, date) {
  return posts
    .filter((post) => post.scheduleAt.startsWith(date))
    .sort(
      (a, b) =>
        a.scheduleAt.localeCompare(b.scheduleAt) || a.id.localeCompare(b.id),
    );
}
