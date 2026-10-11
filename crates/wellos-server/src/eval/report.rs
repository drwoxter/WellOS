//! Evaluation reports: machine-readable JSON, a terse operator summary and a
//! self-contained HTML page (no external assets, no scripts). Every artefact
//! contains typed outputs, hashes and safe metadata only.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use super::rubric::{CaseStatus, Outcome, Verification};
use super::runner::{ExecutionRecord, RunOutcome};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Bucket {
    pub cases: usize,
    pub passed: usize,
    pub needs_review: usize,
    pub failed: usize,
    pub aborted: usize,
    pub fallback_verified: usize,
    pub safety_failures: usize,
}

impl Bucket {
    fn add(&mut self, r: &ExecutionRecord) {
        self.cases += 1;
        match r.status {
            CaseStatus::Passed => self.passed += 1,
            CaseStatus::NeedsReview => self.needs_review += 1,
            CaseStatus::Failed => self.failed += 1,
            CaseStatus::Aborted => self.aborted += 1,
            CaseStatus::ProviderUnavailableFallbackVerified => self.fallback_verified += 1,
        }
        if !r.verdict.safety_failures.is_empty() {
            self.safety_failures += 1;
        }
    }
}

/// Per-metric roll-up across executions.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MetricRollup {
    pub verification: Option<Verification>,
    pub pass: usize,
    pub fail: usize,
    pub review: usize,
    pub not_applicable: usize,
    pub not_validated: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub by_operation: BTreeMap<&'static str, Bucket>,
    pub by_language: BTreeMap<String, Bucket>,
    pub by_metric: BTreeMap<&'static str, MetricRollup>,
    /// Cases where the provider output and the deterministic baseline agree
    /// on the decisive field (hash-equal typed output).
    pub baseline_agreement: usize,
    pub provider_errors: BTreeMap<String, usize>,
}

pub fn summarize(outcome: &RunOutcome) -> Summary {
    let mut by_operation: BTreeMap<&'static str, Bucket> = BTreeMap::new();
    let mut by_language: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_metric: BTreeMap<&'static str, MetricRollup> = BTreeMap::new();
    let mut provider_errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut baseline_agreement = 0;
    for r in &outcome.executions {
        by_operation.entry(r.operation).or_default().add(r);
        by_language.entry(r.language.clone()).or_default().add(r);
        for m in &r.verdict.metrics {
            let e = by_metric.entry(m.metric).or_default();
            e.verification = Some(m.verification);
            match m.outcome {
                Outcome::Pass => e.pass += 1,
                Outcome::Fail => e.fail += 1,
                Outcome::Review => e.review += 1,
                Outcome::NotApplicable => e.not_applicable += 1,
                Outcome::NotValidated => e.not_validated += 1,
            }
        }
        if let Some(code) = &r.failure_code {
            *provider_errors.entry(code.clone()).or_default() += 1;
        }
        if let (Some(p), Some(b)) = (&r.provider_output, &r.baseline_output) {
            if dmind_gateway::hash_json(p) == dmind_gateway::hash_json(b) {
                baseline_agreement += 1;
            }
        }
    }
    Summary {
        by_operation,
        by_language,
        by_metric,
        baseline_agreement,
        provider_errors,
    }
}

/// Full machine-readable report.
pub fn to_json(outcome: &RunOutcome) -> Value {
    serde_json::json!({
        "report_version": "dmind-eval-report.v1",
        "synthetic_only": true,
        "plan": outcome.plan,
        "provider": outcome.provider,
        "totals": outcome.totals,
        "abort": outcome.abort,
        "safety_failures": outcome.safety_failures,
        "summary": summarize(outcome),
        "executions": outcome.executions,
    })
}

/// Terse operator summary (stdout).
pub fn to_text(outcome: &RunOutcome) -> String {
    let t = &outcome.totals;
    let s = summarize(outcome);
    let mut out = String::new();
    out.push_str(&format!(
        "dMind evaluation — suite {}@{} rubric {} mode {} provider {}/{} ({})\n",
        outcome.plan.suite_key,
        outcome.plan.suite_version,
        outcome.plan.rubric_version,
        outcome.plan.mode.as_str(),
        outcome.provider.provider,
        outcome.provider.model,
        if outcome.provider.synthetic_provider {
            "synthetic fixture"
        } else if outcome.provider.external {
            "external"
        } else {
            "internal"
        }
    ));
    out.push_str("SYNTHETIC DATA ONLY — evaluation infrastructure, not clinical validation.\n");
    out.push_str(&format!(
        "cases {} | passed {} | needs_review {} | failed {} | fallback_verified {} | aborted {}\n",
        t.cases, t.passed, t.needs_review, t.failed, t.fallback_verified, t.aborted
    ));
    out.push_str(&format!(
        "provider calls {} | retries {} | tokens in/out {}/{} | est. cost {} {} | latency p50/p95 {}/{} ms\n",
        t.provider_calls,
        t.retries,
        t.input_tokens,
        t.output_tokens,
        t.estimated_cost,
        outcome.plan.budget.pricing.as_ref().map(|p| p.currency.as_str()).unwrap_or("n/a"),
        t.p50_latency_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
        t.p95_latency_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
    ));
    if let Some(a) = &outcome.abort {
        out.push_str(&format!("ABORTED: {a:?}\n"));
    }
    for (op, b) in &s.by_operation {
        out.push_str(&format!(
            "  {op:<30} pass {:>2} review {:>2} fail {:>2} fallback {:>2} safety {:>2}\n",
            b.passed, b.needs_review, b.failed, b.fallback_verified, b.safety_failures
        ));
    }
    for f in &outcome.safety_failures {
        out.push_str(&format!(
            "  SAFETY {} [{}]: {}\n",
            f.case_key,
            f.operation,
            f.metrics.join(", ")
        ));
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Self-contained HTML report (inline CSS, no scripts, no remote assets).
pub fn to_html(outcome: &RunOutcome) -> String {
    let t = &outcome.totals;
    let s = summarize(outcome);
    let mut h = String::new();
    h.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    h.push_str("<title>dMind evaluation report</title><style>");
    h.push_str(
        "body{font:14px/1.45 system-ui,sans-serif;margin:2rem;color:#1b1f24;background:#fff}\
         h1,h2{font-weight:600}table{border-collapse:collapse;width:100%;margin:1rem 0}\
         th,td{border:1px solid #d0d7de;padding:.35rem .5rem;text-align:left;vertical-align:top}\
         th{background:#f6f8fa}.banner{background:#fff4ce;border:1px solid #e3b341;padding:.75rem 1rem;border-radius:6px}\
         .pass{color:#1a7f37}.fail{color:#cf222e}.review{color:#9a6700}.fallback{color:#0969da}\
         code{font-size:12px;background:#f6f8fa;padding:.1rem .3rem;border-radius:4px}\
         details{margin:.5rem 0}pre{white-space:pre-wrap;word-break:break-word;font-size:12px;background:#f6f8fa;padding:.5rem;border-radius:6px}",
    );
    h.push_str("</style></head><body>");
    h.push_str("<p class=\"banner\"><strong>Synthetic data only.</strong> This is evaluation infrastructure for a configured dMind provider. It is not clinical validation, not regulatory evidence and not approval for clinical use; every clinical decision remains with an accountable professional.</p>");
    h.push_str(&format!(
        "<h1>dMind evaluation — {}@{}</h1><p>Rubric <code>{}</code> · corpus hash <code>{}</code> · mode <code>{}</code> · provider <code>{}</code> / <code>{}</code> ({}) · state <code>{}</code></p>",
        esc(outcome.plan.suite_key),
        esc(outcome.plan.suite_version),
        esc(outcome.plan.rubric_version),
        esc(&outcome.plan.corpus_hash[..16.min(outcome.plan.corpus_hash.len())]),
        outcome.plan.mode.as_str(),
        esc(&outcome.provider.provider),
        esc(&outcome.provider.model),
        if outcome.provider.synthetic_provider { "synthetic fixture" } else if outcome.provider.external { "external" } else { "internal" },
        esc(&outcome.provider.state),
    ));
    h.push_str(&format!(
        "<h2>Totals</h2><table><tr><th>Cases</th><th>Passed</th><th>Needs review</th><th>Failed</th><th>Fallback verified</th><th>Aborted</th><th>Provider calls</th><th>Retries</th><th>Tokens in/out</th><th>Est. cost</th><th>Latency p50/p95</th></tr>\
         <tr><td>{}</td><td class=\"pass\">{}</td><td class=\"review\">{}</td><td class=\"fail\">{}</td><td class=\"fallback\">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}/{}</td><td>{} {}</td><td>{}/{} ms</td></tr></table>",
        t.cases, t.passed, t.needs_review, t.failed, t.fallback_verified, t.aborted, t.provider_calls, t.retries,
        t.input_tokens, t.output_tokens, t.estimated_cost,
        outcome.plan.budget.pricing.as_ref().map(|p| esc(&p.currency)).unwrap_or_else(|| "n/a".into()),
        t.p50_latency_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
        t.p95_latency_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
    ));
    if let Some(a) = &outcome.abort {
        h.push_str(&format!(
            "<p class=\"fail\"><strong>Aborted:</strong> <code>{}</code></p>",
            esc(&format!("{a:?}"))
        ));
    }
    h.push_str("<h2>Operation quality matrix</h2><table><tr><th>Operation</th><th>Cases</th><th>Pass</th><th>Review</th><th>Fail</th><th>Fallback</th><th>Safety failures</th></tr>");
    for (op, b) in &s.by_operation {
        h.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td class=\"pass\">{}</td><td class=\"review\">{}</td><td class=\"fail\">{}</td><td class=\"fallback\">{}</td><td class=\"fail\">{}</td></tr>",
            esc(op), b.cases, b.passed, b.needs_review, b.failed, b.fallback_verified, b.safety_failures
        ));
    }
    h.push_str("</table><h2>Language comparison</h2><table><tr><th>Language</th><th>Cases</th><th>Pass</th><th>Review</th><th>Fail</th><th>Fallback</th></tr>");
    for (lang, b) in &s.by_language {
        h.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td class=\"pass\">{}</td><td class=\"review\">{}</td><td class=\"fail\">{}</td><td class=\"fallback\">{}</td></tr>",
            esc(lang), b.cases, b.passed, b.needs_review, b.failed, b.fallback_verified
        ));
    }
    h.push_str("</table><h2>Metrics</h2><table><tr><th>Metric</th><th>Verification</th><th>Pass</th><th>Fail</th><th>Review</th><th>N/A</th><th>Not validated</th></tr>");
    for (m, r) in &s.by_metric {
        h.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td class=\"pass\">{}</td><td class=\"fail\">{}</td><td class=\"review\">{}</td><td>{}</td><td>{}</td></tr>",
            esc(m),
            r.verification.map(|v| format!("{v:?}")).unwrap_or_default(),
            r.pass, r.fail, r.review, r.not_applicable, r.not_validated
        ));
    }
    h.push_str(&format!(
        "</table><p>Provider output hash-equal to deterministic baseline: {} of {} executions. Provider errors: {}</p>",
        s.baseline_agreement,
        outcome.executions.len(),
        if s.provider_errors.is_empty() { "none".to_string() } else {
            s.provider_errors.iter().map(|(k, v)| format!("<code>{}</code>×{v}", esc(k))).collect::<Vec<_>>().join(", ")
        }
    ));
    if !outcome.safety_failures.is_empty() {
        h.push_str("<h2 class=\"fail\">Safety failures</h2><ul>");
        for f in &outcome.safety_failures {
            h.push_str(&format!(
                "<li><code>{}</code> [{}]: {}</li>",
                esc(&f.case_key),
                esc(f.operation),
                esc(&f.metrics.join(", "))
            ));
        }
        h.push_str("</ul>");
    }
    h.push_str("<h2>Executions</h2>");
    for r in &outcome.executions {
        let cls = match r.status {
            CaseStatus::Passed => "pass",
            CaseStatus::NeedsReview => "review",
            CaseStatus::Failed => "fail",
            CaseStatus::Aborted => "",
            CaseStatus::ProviderUnavailableFallbackVerified => "fallback",
        };
        h.push_str(&format!(
            "<details><summary><code>{}</code> {} · {} · <span class=\"{cls}\">{:?}</span> · {} · {} ms</summary>",
            esc(&r.case_key), esc(r.operation), esc(&r.language), r.status, r.execution_kind.as_str(),
            r.latency_ms.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
        ));
        h.push_str("<table><tr><th>Metric</th><th>Verification</th><th>Outcome</th><th>Severity</th><th>Detail</th></tr>");
        for m in &r.verdict.metrics {
            let c = match m.outcome {
                Outcome::Pass => "pass",
                Outcome::Fail => "fail",
                Outcome::Review => "review",
                _ => "",
            };
            h.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{:?}</td><td class=\"{c}\">{:?}</td><td>{:?}</td><td>{}</td></tr>",
                esc(m.metric), m.verification, m.outcome, m.severity, esc(&m.detail)
            ));
        }
        h.push_str("</table>");
        h.push_str(&format!("<p>input hash <code>{}</code> · prompt <code>{}</code> · schema <code>{}</code> · baseline hash <code>{}</code></p>",
            esc(&r.input_hash), esc(&r.prompt_version), esc(r.schema_version), esc(&r.baseline_hash)));
        if let Some(p) = &r.provider_output {
            h.push_str(&format!(
                "<p>Provider structured output</p><pre>{}</pre>",
                esc(&serde_json::to_string_pretty(p).unwrap_or_default())
            ));
        }
        if let Some(b) = &r.baseline_output {
            h.push_str(&format!(
                "<p>Deterministic baseline</p><pre>{}</pre>",
                esc(&serde_json::to_string_pretty(b).unwrap_or_default())
            ));
        }
        h.push_str("</details>");
    }
    h.push_str("</body></html>");
    h
}
