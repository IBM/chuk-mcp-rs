//! Executing the rule table and reporting the result.

use std::fmt::Write as _;

use crate::rule::{Era, Rule, Subject};

/// Column width for the rule id in the rendered matrix. Wide enough for the
/// longest id in the table; ids longer than this simply push their row wider.
const ID_COLUMN_WIDTH: usize = 44;
const ERA_COLUMN_WIDTH: usize = 11;
const SUBJECT_COLUMN_WIDTH: usize = 9;

const PASS_MARK: &str = "pass";
const FAIL_MARK: &str = "FAIL";

/// What one rule did.
pub struct Outcome {
    pub id: &'static str,
    pub era: Era,
    pub subject: Subject,
    pub requirement: &'static str,
    /// `None` when the rule held.
    pub failure: Option<String>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

/// Every rule's outcome, plus the rendering the suite reports.
pub struct Report {
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn failures(&self) -> Vec<&Outcome> {
        self.outcomes.iter().filter(|o| !o.passed()).collect()
    }

    pub fn passed(&self) -> usize {
        self.outcomes.iter().filter(|o| o.passed()).count()
    }

    /// The coverage matrix: one line per rule, grouped by subject then era.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:<ID_COLUMN_WIDTH$} {:<ERA_COLUMN_WIDTH$} {:<SUBJECT_COLUMN_WIDTH$} RESULT",
            "RULE", "ERA", "SUBJECT"
        );
        for outcome in &self.outcomes {
            let mark = if outcome.passed() {
                PASS_MARK
            } else {
                FAIL_MARK
            };
            let _ = writeln!(
                out,
                "{:<ID_COLUMN_WIDTH$} {:<ERA_COLUMN_WIDTH$} {:<SUBJECT_COLUMN_WIDTH$} {}",
                outcome.id,
                outcome.era.to_string(),
                outcome.subject.to_string(),
                mark
            );
        }
        let _ = writeln!(
            out,
            "\n{} of {} rules held.",
            self.passed(),
            self.outcomes.len()
        );
        out
    }

    /// The detail a failing run needs: what was required, and what happened.
    pub fn render_failures(&self) -> String {
        let mut out = String::new();
        for outcome in self.failures() {
            let _ = writeln!(
                out,
                "\n{} [{} {}]",
                outcome.id, outcome.era, outcome.subject
            );
            let _ = writeln!(out, "  required: {}", outcome.requirement);
            let _ = writeln!(
                out,
                "  observed: {}",
                outcome.failure.as_deref().unwrap_or("")
            );
        }
        out
    }
}

/// Run every rule. Rules run in table order and never short-circuit: a
/// conformance run reports the whole picture, not the first thing that broke.
pub async fn run(rules: Vec<Rule>) -> Report {
    let mut outcomes = Vec::with_capacity(rules.len());
    for rule in rules {
        let failure = (rule.check)().await.err();
        outcomes.push(Outcome {
            id: rule.id,
            era: rule.era,
            subject: rule.subject,
            requirement: rule.requirement,
            failure,
        });
    }
    Report { outcomes }
}
