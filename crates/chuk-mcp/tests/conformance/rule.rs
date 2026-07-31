//! The rule model.
//!
//! A conformance rule is one thing the specification requires, expressed as
//! data: which era it belongs to, what it constrains, the requirement in one
//! line, and a check. Rules are values rather than `#[test]` functions so the
//! suite can be counted, filtered by era, and rendered as a coverage matrix —
//! which is the question a conformance suite exists to answer.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// Which protocol generation a rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// `2025-11-25` and earlier: `initialize`, sessions, `ping`.
    Legacy,
    /// `2026-07-28`: stateless requests, `_meta`, mirrored headers.
    Modern,
    /// Required identically of both.
    Both,
}

impl fmt::Display for Era {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Era::Legacy => "legacy",
            Era::Modern => "2026-07-28",
            Era::Both => "both",
        })
    }
}

/// What the rule constrains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// What our client puts on the wire.
    Client,
    /// How our server answers.
    Server,
    /// Shape rules either side must honour.
    Protocol,
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Subject::Client => "client",
            Subject::Server => "server",
            Subject::Protocol => "protocol",
        })
    }
}

/// A check's verdict. The failure carries an explanation, not a boolean: a
/// conformance failure that does not say what the peer would have seen is not
/// actionable.
pub type Verdict = Result<(), String>;

type CheckFuture = Pin<Box<dyn Future<Output = Verdict> + Send>>;

/// A rule's check, boxed so async and synchronous rules can share one table.
pub type Check = Box<dyn Fn() -> CheckFuture + Send + Sync>;

/// One requirement of the specification.
pub struct Rule {
    /// Stable identifier, used in reports and failure messages.
    pub id: &'static str,
    pub era: Era,
    pub subject: Subject,
    /// The requirement, phrased as the spec phrases it.
    pub requirement: &'static str,
    pub check: Check,
}

impl Rule {
    /// Build a rule from an async check.
    pub fn new<F, Fut>(
        id: &'static str,
        era: Era,
        subject: Subject,
        requirement: &'static str,
        check: F,
    ) -> Rule
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Verdict> + Send + 'static,
    {
        Rule {
            id,
            era,
            subject,
            requirement,
            check: Box::new(move || Box::pin(check())),
        }
    }

    /// Build a rule from a synchronous check, which most protocol-shape rules
    /// are.
    pub fn sync<F>(
        id: &'static str,
        era: Era,
        subject: Subject,
        requirement: &'static str,
        check: F,
    ) -> Rule
    where
        F: Fn() -> Verdict + Send + Sync + Copy + 'static,
    {
        Rule::new(
            id,
            era,
            subject,
            requirement,
            move || async move { check() },
        )
    }
}

/// Assert equality, reporting both sides. Used pervasively by checks, where a
/// bare `assert_eq!` would abort the run instead of recording a verdict.
pub fn expect_eq<T: PartialEq + fmt::Debug>(what: &str, actual: T, expected: T) -> Verdict {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{what}: expected {expected:?}, got {actual:?}"))
    }
}

/// Assert a condition, reporting the supplied explanation when it does not
/// hold.
pub fn expect(condition: bool, explanation: impl Into<String>) -> Verdict {
    if condition {
        Ok(())
    } else {
        Err(explanation.into())
    }
}
