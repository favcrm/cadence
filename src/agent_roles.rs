//! CAD-1752: the built-in role catalogue a hire copies from.
//!
//! A role is a recipe, not a clone: hiring copies its `instructions` into
//! the new agent and records the catalogue id in `params`. Logins, keys,
//! MCP servers, grants and history are never part of a role. `team_role`
//! only routes the model (it must be one of `model_defaults::TEAM_ROLES`).

use serde_json::{json, Value};

/// `AgentAvatar` look slugs (`ui/src/features/agents/AgentAvatar.tsx`).
pub const LOOKS: &[&str] = &[
    "builder",
    "guard",
    "bookkeeper",
    "content",
    "designer",
    "reception",
    "operations",
    "support",
    "tester",
    "robot",
    "custom",
];

pub const CATEGORIES: &[(&str, &str)] = &[
    ("tech", "Tech"),
    ("business", "Business"),
    ("customer", "Customer"),
];

pub struct Role {
    pub id: &'static str,
    pub category: &'static str,
    pub title: &'static str,
    pub look: &'static str,
    pub team_role: &'static str,
    /// `tickets` works repo tickets; `apps` works through installed apps.
    pub kind: &'static str,
    pub job: &'static str,
    pub does: &'static [&'static str],
    pub skills: &'static [&'static str],
    pub tools: &'static [&'static str],
    pub first_tasks: &'static [&'static str],
}

impl Role {
    /// The text a hire stores as the agent's instructions: the job line
    /// plus the `does` bullets.
    pub fn instructions(&self) -> String {
        let article = if self.title.starts_with(['A', 'E', 'I', 'O', 'U']) {
            "an"
        } else {
            "a"
        };
        let mut text = format!(
            "You are {article} {}. {}\n\nWhat you do:\n",
            self.title, self.job
        );
        for line in self.does {
            text.push_str("- ");
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    /// The catalogue row without the install-dependent `model`.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "category": self.category, "title": self.title,
            "look": self.look, "team_role": self.team_role, "kind": self.kind,
            "job": self.job, "does": self.does, "skills": self.skills,
            "tools": self.tools, "first_tasks": self.first_tasks,
            "instructions": self.instructions(),
        })
    }
}

/// The valid role ids, for refusals that name them.
pub fn ids() -> String {
    ROLES.iter().map(|r| r.id).collect::<Vec<_>>().join(", ")
}

pub fn find(id: &str) -> Option<&'static Role> {
    ROLES.iter().find(|role| role.id == id)
}

/// The catalogue's own spelling of a look, if it is one.
pub fn canonical_look(look: &str) -> Option<&'static str> {
    LOOKS.iter().copied().find(|known| *known == look)
}

pub fn valid_look(look: &str) -> bool {
    canonical_look(look).is_some()
}

/// The stable alias → avatar look assignment, matching the board's
/// `AgentAvatar.tsx::lookFor`: `(hash * 31 + code unit) >>> 0`, modulo the
/// catalogue. Literal aliases are ASCII, so code points and code units agree.
pub fn look_for_alias(alias: &str) -> &'static str {
    let mut hash: u32 = 0;
    for ch in alias.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(ch) & 0xffff);
    }
    LOOKS[(hash % LOOKS.len() as u32) as usize]
}

/// The effective look: a canonical pin wins, Master defaults to the existing
/// robot when unpinned, and every other alias wears its stable alias look.
pub fn effective_look(alias: &str, pinned: Option<&str>) -> &'static str {
    if let Some(look) = pinned.and_then(canonical_look) {
        return look;
    }
    if alias == crate::master::ALIAS {
        "robot"
    } else {
        look_for_alias(alias)
    }
}

/// Is this agent a catalogue hire? Its launch params carry `catalogue`.
pub fn is_catalogue_hire(params: Option<&Value>) -> bool {
    params.is_some_and(|p| p.get("catalogue").is_some())
}

pub const ROLES: &[Role] = &[
    Role {
        id: "developer",
        category: "tech",
        title: "Developer",
        look: "builder",
        team_role: "dev",
        kind: "tickets",
        job: "Builds features and fixes bugs, then opens a pull request.",
        does: &[
            "Reads the ticket and the code",
            "Makes the change in the ticket's own branch",
            "Opens a PR and answers review",
        ],
        skills: &["cadence"],
        tools: &[
            "Repo of the ticket (files, terminal)",
            "GitHub: push branch, open PR",
        ],
        first_tasks: &[
            "Fix the newest bug ticket",
            "Add a dark-mode toggle",
            "Explain how login works",
        ],
    },
    Role {
        id: "designer",
        category: "tech",
        title: "Designer",
        look: "designer",
        team_role: "worker",
        kind: "tickets",
        job: "Turns an idea into a clickable mockup using your design kit.",
        does: &[
            "Reads the design kit and live screens",
            "Drafts a mockup page",
            "Shares a preview link for comments",
        ],
        skills: &["cadence", "website-builder"],
        tools: &[
            "Repo of the ticket (design kit)",
            "Canvas: publish a preview",
        ],
        first_tasks: &[
            "Mock a pricing page",
            "Tidy the settings screen",
            "Make a phone layout",
        ],
    },
    Role {
        id: "pm",
        category: "tech",
        title: "Project manager",
        look: "operations",
        team_role: "pm",
        kind: "apps",
        job: "Breaks goals into tickets, assigns the team, and chases what is stuck.",
        does: &[
            "Writes tickets with clear acceptance",
            "Hands work to the right agent",
            "Posts a daily summary",
        ],
        skills: &["cadence"],
        tools: &["Board: tickets and threads", "Message other agents"],
        first_tasks: &[
            "Plan this week",
            "What is blocked?",
            "Split this idea into tickets",
        ],
    },
    Role {
        id: "qa",
        category: "tech",
        title: "QA tester",
        look: "tester",
        team_role: "qa",
        kind: "tickets",
        job: "Clicks through the app like a customer and reports what breaks.",
        does: &[
            "Tests a PR on desktop and phone",
            "Takes screenshots as proof",
            "Files a clear bug with steps",
        ],
        skills: &["cadence"],
        tools: &["Repo of the ticket", "Test browser"],
        first_tasks: &[
            "Test the newest PR",
            "Check sign-up on a phone",
            "Smoke-test checkout",
        ],
    },
    Role {
        id: "devops",
        category: "tech",
        title: "DevOps",
        look: "guard",
        team_role: "devops",
        kind: "tickets",
        job: "Keeps builds green and environments healthy.",
        does: &[
            "Reads failing CI runs",
            "Fixes build scripts",
            "Watches disk and services",
        ],
        skills: &["cadence"],
        tools: &["Repo of the ticket", "CI logs"],
        first_tasks: &["Why is CI red?", "Speed up the build", "Check disk space"],
    },
    Role {
        id: "architect",
        category: "tech",
        title: "Architect",
        look: "robot",
        team_role: "architect",
        kind: "tickets",
        job: "Reviews designs and big changes before anyone builds them.",
        does: &[
            "Reads the code base",
            "Writes a short design note",
            "Flags risky changes",
        ],
        skills: &["cadence"],
        tools: &["Repo of the ticket (read)"],
        first_tasks: &[
            "Review this plan",
            "Map the auth flow",
            "Where should this live?",
        ],
    },
    Role {
        id: "accountant",
        category: "business",
        title: "Accountant",
        look: "bookkeeper",
        team_role: "worker",
        kind: "apps",
        job: "Reconciles invoices and payments and prepares a monthly summary.",
        does: &[
            "Matches invoices to payments",
            "Flags missing or odd entries",
            "Drafts the month-end summary",
        ],
        skills: &["business-documents", "company-outputs"],
        tools: &["Invoices app (read)", "CRM orders (read)"],
        first_tasks: &[
            "Reconcile October",
            "List unpaid invoices",
            "Draft the monthly summary",
        ],
    },
    Role {
        id: "researcher",
        category: "business",
        title: "Researcher",
        look: "content",
        team_role: "research",
        kind: "apps",
        job: "Researches a market, competitor or question and writes a sourced brief.",
        does: &[
            "Searches the web and your wiki",
            "Compares sources",
            "Writes a brief with links",
        ],
        skills: &["company-outputs"],
        tools: &["Web search", "Wiki (read, write own notes)"],
        first_tasks: &[
            "Compare 3 competitors",
            "Size this market",
            "Summarise this report",
        ],
    },
    Role {
        id: "quotation",
        category: "business",
        title: "Quotation",
        look: "operations",
        team_role: "worker",
        kind: "apps",
        job: "Turns a customer request into a priced quote from your price list.",
        does: &[
            "Reads the request and price list",
            "Drafts a PDF quote",
            "Asks you before it is sent",
        ],
        skills: &["business-documents"],
        tools: &["CRM (read)", "Document templates"],
        first_tasks: &[
            "Quote for Acme, 40 seats",
            "Update a quote",
            "Explain this discount",
        ],
    },
    Role {
        id: "analyst",
        category: "business",
        title: "Analyst",
        look: "custom",
        team_role: "research",
        kind: "apps",
        job: "Answers business questions from your data with a chart and a short why.",
        does: &[
            "Pulls numbers from connected apps",
            "Builds a chart or table",
            "Explains what changed",
        ],
        skills: &["company-outputs"],
        tools: &["App data (read)", "Canvas: charts and reports"],
        first_tasks: &[
            "Why did sales dip?",
            "Top 10 customers",
            "Weekly KPI report",
        ],
    },
    Role {
        id: "support",
        category: "customer",
        title: "Support",
        look: "support",
        team_role: "worker",
        kind: "apps",
        job: "Drafts replies to customer messages from your help docs.",
        does: &[
            "Reads the inbox and help docs",
            "Drafts a reply",
            "Escalates what it cannot answer",
        ],
        skills: &["company-outputs"],
        tools: &["Inbox: email / WhatsApp (read, draft)", "Wiki (read)"],
        first_tasks: &[
            "Reply to today's emails",
            "Find common questions",
            "Write an FAQ",
        ],
    },
    Role {
        id: "sales",
        category: "customer",
        title: "Sales follow-up",
        look: "reception",
        team_role: "worker",
        kind: "apps",
        job: "Keeps leads warm: reminds you who to call and drafts follow-ups.",
        does: &[
            "Reads the CRM pipeline",
            "Spots leads going cold",
            "Drafts a follow-up",
        ],
        skills: &["company-outputs"],
        tools: &["CRM (read)", "Inbox (draft)"],
        first_tasks: &[
            "Who needs a follow-up?",
            "Draft 5 follow-ups",
            "Summarise the pipeline",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_is_registrable() {
        assert_eq!(ROLES.len(), 12);
        for (i, role) in ROLES.iter().enumerate() {
            assert!(
                crate::model_defaults::canonical_team_role(role.team_role),
                "{} team role {}",
                role.id,
                role.team_role
            );
            assert!(valid_look(role.look), "{} look {}", role.id, role.look);
            assert!(CATEGORIES.iter().any(|(id, _)| *id == role.category));
            assert!(matches!(role.kind, "tickets" | "apps"));
            assert!(role.instructions().len() <= crate::store::MAX_INSTRUCTIONS_BYTES);
            assert!(ROLES[i + 1..].iter().all(|other| other.id != role.id));
        }
    }

    /// CAD-1767: an article before a vowel, and refusals can name the ids.
    #[test]
    fn instructions_use_the_right_article_and_ids_list_every_role() {
        assert!(find("accountant")
            .unwrap()
            .instructions()
            .starts_with("You are an Accountant."));
        assert!(find("developer")
            .unwrap()
            .instructions()
            .starts_with("You are a Developer."));
        let ids = ids();
        assert!(ROLES.iter().all(|r| ids.contains(r.id)));
    }

    /// CAD-1817: the board's wire enum and the UI's palette list stay the same
    /// eleven slugs, and an unpinned master keeps the existing robot.
    #[test]
    fn avatar_look_contract_matches_the_catalogue() {
        for look in LOOKS {
            let parsed = cadence_core::board_api::AvatarLook::parse(look)
                .unwrap_or_else(|| panic!("wire AvatarLook lacks '{look}'"));
            assert_eq!(parsed.as_str(), *look);
        }
        assert!(cadence_core::board_api::AvatarLook::parse("dragon").is_none());
        assert!(valid_look(look_for_alias("assist-0123456789abcdef")));
        assert_eq!(effective_look("master", None), "robot");
        assert_eq!(effective_look("assist-x", Some("support")), "support");
        assert_eq!(
            effective_look("assist-x", Some("dragon")),
            look_for_alias("assist-x")
        );
    }
}
