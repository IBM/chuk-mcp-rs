//! The in-repo MCP protocol conformance suite.
//!
//! Answers one question: does this implementation do what the specification
//! requires, in each protocol era? Rules are data (see [`rule`]), so the suite
//! reports a coverage matrix rather than a pass/fail count — which is what
//! makes an unimplemented area visible instead of merely untested.
//!
//! ```text
//! cargo test -p chuk-mcp --test conformance -- --nocapture
//! ```
//!
//! This complements the official `@modelcontextprotocol/conformance` suite
//! wired up in `scripts/run-conformance.sh`: that one exercises our client as
//! a black box against the reference scenarios, this one asserts the rules the
//! reference suite does not yet cover — everything in the `2026-07-28`
//! revision, and our server's behaviour.

mod harness;
mod rule;
mod rules;
mod runner;

use rule::{Era, Subject};

/// The suite. One test, because a conformance run reports the whole matrix:
/// splitting it into per-rule tests would trade that report for a list of
/// names, and hide how much of the specification is covered at all.
#[tokio::test]
async fn protocol_conformance() {
    let report = runner::run(rules::all()).await;
    println!("\n{}", report.render());
    println!("{}", coverage_summary(&report));

    assert!(
        report.failures().is_empty(),
        "{} conformance rule(s) did not hold:\n{}\n{}",
        report.failures().len(),
        report.render_failures(),
        report.render(),
    );
}

/// How many rules cover each era and subject. A count of zero is the useful
/// signal here — it names an area the suite does not yet speak to.
fn coverage_summary(report: &runner::Report) -> String {
    let mut lines = vec!["Coverage:".to_string()];
    for era in [Era::Legacy, Era::Modern, Era::Both] {
        for subject in [Subject::Client, Subject::Server, Subject::Protocol] {
            let count = report
                .outcomes
                .iter()
                .filter(|outcome| outcome.era == era && outcome.subject == subject)
                .count();
            if count > 0 {
                // Rendered via `to_string` because the width applies to a
                // `&str`, not to a Display impl that writes straight out.
                lines.push(format!(
                    "  {:<11} {:<9} {count} rules",
                    era.to_string(),
                    subject.to_string()
                ));
            }
        }
    }
    lines.join("\n")
}
