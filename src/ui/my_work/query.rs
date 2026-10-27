//! Query, ordering and cursor parsing for the My work view.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Active,
    Done,
    Dropped,
    All,
}

impl Tab {
    pub(crate) fn holds(self, status: &str) -> bool {
        match self {
            Tab::Active => ACTIVE.contains(&status),
            Tab::Done => status == "done",
            Tab::Dropped => status == "dropped",
            Tab::All => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sort {
    Updated,
    Priority,
    Created,
}

/// A row's place in the page order; rows sort ascending by it. The cursor
/// is the last shown row's key, so it names a position, never an offset.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RowKey {
    /// Active tab: the status group (doing, review, ready, backlog).
    pub(crate) group: u8,
    /// Priority sort: P0 = 0 … P3 = 3.
    pub(crate) rank: u8,
    /// The sort clock negated, so newest sorts first; unknown sorts last.
    pub(crate) newest: i64,
    pub(crate) prefix: String,
    /// The id number negated: newer ids first among equals.
    pub(crate) number: i64,
}

impl RowKey {
    pub(crate) fn encode(&self) -> String {
        format!(
            "{}.{}.{}.{}.{}",
            self.group, self.rank, self.newest, self.prefix, self.number
        )
    }

    pub(crate) fn decode(raw: &str) -> Option<Self> {
        let mut parts = raw.split('.');
        let key = Self {
            group: parts.next()?.parse().ok()?,
            rank: parts.next()?.parse().ok()?,
            newest: parts.next()?.parse().ok()?,
            prefix: parts.next()?.to_string(),
            number: parts.next()?.parse().ok()?,
        };
        let prefix_ok = key.prefix.len() <= 16
            && key
                .prefix
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        (parts.next().is_none() && prefix_ok).then_some(key)
    }
}

/// The board read's closed parameter set. No owner, subject, role or
/// selector of another person: the session alone names "my".
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Query {
    pub(crate) tab: Tab,
    /// Lowercased; matched against the id and the title.
    pub(crate) search: Option<String>,
    pub(crate) project: Option<String>,
    pub(crate) priority: Option<String>,
    pub(crate) blocked: bool,
    pub(crate) sort: Sort,
    pub(crate) after: Option<RowKey>,
    pub(crate) limit: usize,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            tab: Tab::Active,
            search: None,
            project: None,
            priority: None,
            blocked: false,
            sort: Sort::Updated,
            after: None,
            limit: PAGE_DEFAULT,
        }
    }
}

impl Query {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let mut query = Self::default();
        let mut seen = BTreeSet::new();
        for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
            let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| crate::ui::pct_decode(&s.replace('+', " "));
            let (Some(key), Some(value)) = (decode(raw_key), decode(raw_value)) else {
                return Err("malformed My work query".to_string());
            };
            if !seen.insert(key.clone()) {
                return Err(format!("My work query repeats {key}"));
            }
            match key.as_str() {
                "tab" => {
                    query.tab = match value.as_str() {
                        "active" => Tab::Active,
                        "done" => Tab::Done,
                        "dropped" => Tab::Dropped,
                        "all" => Tab::All,
                        _ => return Err("tab must be active, done, dropped or all".to_string()),
                    }
                }
                "q" => {
                    let text = value.trim();
                    if text.chars().count() > SEARCH_MAX_CHARS {
                        return Err(format!("q is longer than {SEARCH_MAX_CHARS} characters"));
                    }
                    query.search = (!text.is_empty()).then(|| text.to_lowercase());
                }
                "project" => {
                    if !model::valid_key(&value) {
                        return Err("project must be a project key".to_string());
                    }
                    query.project = Some(value);
                }
                "priority" => {
                    if !model::PRIORITIES.contains(&value.as_str()) {
                        return Err("priority must be P0, P1, P2 or P3".to_string());
                    }
                    query.priority = Some(value);
                }
                "blocked" => {
                    if value != "1" {
                        return Err("blocked must be 1".to_string());
                    }
                    query.blocked = true;
                }
                "sort" => {
                    query.sort = match value.as_str() {
                        "updated" => Sort::Updated,
                        "priority" => Sort::Priority,
                        "created" => Sort::Created,
                        _ => return Err("sort must be updated, priority or created".to_string()),
                    }
                }
                "cursor" => {
                    query.after =
                        Some(RowKey::decode(&value).ok_or("cursor is not a My work cursor")?);
                }
                "limit" => {
                    query.limit = number(&value)
                        .filter(|limit| (1..=PAGE_MAX).contains(limit))
                        .ok_or(format!("limit must be 1 to {PAGE_MAX}"))?;
                }
                _ => return Err(format!("My work does not accept {key}")),
            }
        }
        Ok(query)
    }

    /// Every filter except the tab.
    pub(crate) fn matches(&self, view: &View) -> bool {
        let front = &view.issue.front;
        self.project
            .as_deref()
            .is_none_or(|project| view.issue.project == project)
            && self
                .priority
                .as_deref()
                .is_none_or(|priority| front.priority == priority)
            && (!self.blocked || view.blocked)
            && self.search.as_deref().is_none_or(|needle| {
                front.id.to_lowercase().contains(needle)
                    || front.title.to_lowercase().contains(needle)
            })
    }

    /// Active rows keep their status groups together so a page never
    /// interleaves them; then the chosen sort; then newest id first.
    pub(crate) fn key(&self, view: &View, updated: Option<i64>) -> RowKey {
        let front = &view.issue.front;
        let newest = |clock: Option<i64>| clock.map_or(i64::MAX, |at| -at);
        let group = if self.tab == Tab::Active {
            ACTIVE
                .iter()
                .position(|status| *status == view.status)
                .unwrap_or(ACTIVE.len()) as u8
        } else {
            0
        };
        let rank = match self.sort {
            Sort::Priority => model::PRIORITIES
                .iter()
                .position(|priority| *priority == front.priority)
                .unwrap_or(model::PRIORITIES.len()) as u8,
            Sort::Updated | Sort::Created => 0,
        };
        let clock = match self.sort {
            Sort::Updated | Sort::Priority => updated,
            Sort::Created => time::parse_iso(&front.created),
        };
        let (prefix, number) = natural_key(&front.id);
        RowKey {
            group,
            rank,
            newest: newest(clock),
            prefix,
            number: -(number.min(i64::MAX as u64) as i64),
        }
    }
}

fn number(value: &str) -> Option<usize> {
    if value.is_empty() || value.len() > 7 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}
