//! SARIF (Static Analysis Results Interchange Format) 2.x logs: runs, tools, rules, results.
use super::records::{Rows, clip, join, nodes, s, span, str_after};
use crate::inspect::{
    Inspection,
    jsondoc::Node,
    spec::{F, JsonSpec},
};

pub static SARIF: JsonSpec = JsonSpec {
    family: "SARIF",
    kind: "Static analysis results",
    matches: |keys, head| {
        keys.iter().any(|k| k == "runs")
            && (str_after(head, "$schema").is_some_and(|v| v.to_lowercase().contains("sarif"))
                || str_after(head, "version").is_some_and(|v| v.starts_with("2.")))
    },
    version: |root| {
        let v = super::records::s(root, "version");
        (!v.is_empty()).then_some(v)
    },
    facts: &[
        F("Log", "Schema", "$schema"),
        F("Log", "Runs", "count:runs"),
    ],
    tables: &[],
    custom: Some(custom),
};

/// `{0}` placeholders replaced by the message's arguments.
fn message(m: &Node, rule: Option<&Node>) -> String {
    let mut text = s(m, "text | markdown");
    if text.is_empty() {
        // A message by id, from the rule's message strings.
        let id = s(m, "id");
        if let (false, Some(rule)) = (id.is_empty(), rule) {
            text = s(rule, &format!("messageStrings/{id}/text"));
        }
    }
    let args = nodes(m, "arguments");
    for (i, a) in args.iter().enumerate().take(20) {
        text = text.replace(&format!("{{{i}}}"), &a.text());
    }
    text
}

/// The rule a result refers to, by index or by id.
fn rule_of<'a>(rules: &[&'a Node], res: &Node) -> Option<&'a Node> {
    let index = s(res, "ruleIndex | rule/index");
    if let Some(r) = index.parse::<usize>().ok().and_then(|i| rules.get(i)) {
        return Some(*r);
    }
    let id = s(res, "ruleId | rule/id");
    rules.iter().copied().find(|r| s(r, "id") == id)
}

fn custom(root: &Node, out: &mut Inspection) {
    let runs = nodes(root, "runs");
    let multi = runs.len() > 1;
    let mut results = Rows::new(
        "Results",
        &[
            "Run",
            "Rule ID",
            "Level",
            "Message",
            "File",
            "Start line",
            "Start column",
            "Kind",
            "Baseline",
        ],
    );
    let mut rules_t = Rows::new(
        "Rules",
        &[
            "Run",
            "ID",
            "Name",
            "Short description",
            "Default level",
            "Help URI (not opened)",
        ],
    );
    let mut levels: Vec<(String, u64)> = vec![];
    let mut files: Vec<String> = vec![];
    let mut invocations_failed = 0;
    let mut invocations = 0;
    for (ri, run) in runs.iter().enumerate() {
        let group = if multi {
            format!("Run {}", ri + 1)
        } else {
            "Tool".into()
        };
        let driver = run.get("tool").and_then(|t| t.get("driver"));
        if let Some(d) = driver {
            out.fact(&group, "Tool", s(d, "name"), span(d));
            out.fact(
                &group,
                "Tool version",
                s(d, "semanticVersion | version"),
                None,
            );
            out.fact(&group, "Organization", s(d, "organization"), None);
            out.fact(
                &group,
                "Information URI (not opened)",
                s(d, "informationUri"),
                None,
            );
        }
        let ext: Vec<String> = nodes(run, "tool/extensions/name")
            .into_iter()
            .map(|n| n.text())
            .collect();
        out.fact(&group, "Extensions", join(&ext, ", "), None);
        for inv in nodes(run, "invocations") {
            invocations += 1;
            let ok = s(inv, "executionSuccessful");
            if ok == "false" {
                invocations_failed += 1;
            }
            out.fact(
                &group,
                "Invocation",
                join(
                    &[
                        match ok.as_str() {
                            "true" => "successful".into(),
                            "false" => "failed".into(),
                            _ => String::new(),
                        },
                        {
                            let c = s(inv, "exitCode");
                            if c.is_empty() {
                                c
                            } else {
                                format!("exit code {c}")
                            }
                        },
                        s(inv, "startTimeUtc"),
                    ],
                    ", ",
                ),
                span(inv),
            );
            for n in nodes(inv, "toolExecutionNotifications")
                .into_iter()
                .take(50)
            {
                out.notes.push(format!(
                    "Tool notification ({}): {}",
                    s(n, "level"),
                    clip(&message(n.get("message").unwrap_or(n), None), 300)
                ));
            }
        }
        let rules = driver.map(|d| nodes(d, "rules")).unwrap_or_default();
        for r in &rules {
            let level = s(r, "defaultConfiguration/level");
            rules_t.push(
                vec![
                    if multi {
                        (ri + 1).to_string()
                    } else {
                        String::new()
                    },
                    s(r, "id"),
                    s(r, "name"),
                    s(r, "shortDescription/text | fullDescription/text"),
                    if level.is_empty() {
                        "warning (default)".into()
                    } else {
                        level
                    },
                    s(r, "helpUri"),
                ],
                span(r),
            );
        }
        for res in nodes(run, "results") {
            let rule = rule_of(&rules, res);
            let kind = s(res, "kind");
            let mut level = s(res, "level");
            if level.is_empty() {
                level = if !kind.is_empty() && kind != "fail" {
                    "none (not a failure)".into()
                } else {
                    let d = rule
                        .map(|r| s(r, "defaultConfiguration/level"))
                        .unwrap_or_default();
                    if d.is_empty() {
                        "warning (default)".into()
                    } else {
                        format!("{d} (rule default)")
                    }
                };
            }
            let key = level.split(' ').next().unwrap_or("").to_owned();
            match levels.iter_mut().find(|(l, _)| *l == key) {
                Some((_, n)) => *n += 1,
                None => levels.push((key, 1)),
            }
            let loc = nodes(res, "locations/physicalLocation").into_iter().next();
            let uri = loc
                .map(|l| {
                    let uri = s(l, "artifactLocation/uri");
                    let base = s(l, "artifactLocation/uriBaseId");
                    if base.is_empty() || uri.contains("://") {
                        uri
                    } else {
                        format!("%{base}%/{uri}")
                    }
                })
                .unwrap_or_default();
            if !uri.is_empty() && !files.contains(&uri) && files.len() < 100_000 {
                files.push(uri.clone());
            }
            results.push(
                vec![
                    if multi {
                        (ri + 1).to_string()
                    } else {
                        String::new()
                    },
                    s(res, "ruleId | rule/id"),
                    level,
                    clip(
                        &res.get("message")
                            .map(|m| message(m, rule))
                            .unwrap_or_default(),
                        500,
                    ),
                    uri,
                    loc.map(|l| s(l, "region/startLine")).unwrap_or_default(),
                    loc.map(|l| s(l, "region/startColumn")).unwrap_or_default(),
                    kind,
                    s(res, "baselineState"),
                ],
                span(res),
            );
        }
        let artifacts = nodes(run, "artifacts").len();
        if artifacts > 0 {
            out.fact(&group, "Artifacts", artifacts.to_string(), None);
        }
    }
    out.fact("Results", "Results", results.seen().to_string(), None);
    for (level, n) in &levels {
        out.fact("Results by level", level, n.to_string(), None);
    }
    out.fact("Results", "Files affected", files.len().to_string(), None);
    if invocations > 0 {
        out.fact(
            "Results",
            "Invocations successful",
            format!("{} of {invocations}", invocations - invocations_failed),
            None,
        );
    }
    results.finish(out);
    rules_t.finish(out);
    out.notes.push(
        "Results are shown as the tool reported them; the analysed files and URIs are not opened"
            .into(),
    );
}
