//! HTTP Archive (HAR 1.2) files: a browser's record of requests and responses. Nothing is
//! fetched: URLs are shown as text. Credentials in `Authorization` headers are not shown.
use super::doc_common::days_from_civil;
use super::records::{Rows, clip, join, nodes, outline, s, span, str_after};
use crate::inspect::{Inspection, Span, Visual, jsondoc::Node, spec::JsonSpec};

pub static HAR: JsonSpec = JsonSpec {
    family: "HAR",
    kind: "HTTP archive (recorded web requests)",
    matches: |keys, head| {
        keys.len() == 1
            && keys[0] == "log"
            && head.contains("\"entries\"")
            && (head.contains("\"creator\"") || str_after(head, "version").is_some())
    },
    version: |root| {
        let v = s(root, "log/version");
        (!v.is_empty()).then(|| format!("HAR {v}"))
    },
    facts: &[],
    tables: &[],
    custom: Some(custom),
};

/// A number of milliseconds or bytes; HAR writes -1 for "not applicable".
fn known(v: &str) -> String {
    if v.starts_with('-') {
        String::new()
    } else {
        v.to_owned()
    }
}

/// Requests drawn in the waterfall; the tables keep them all.
const MAX_WATERFALL: usize = 5_000;
/// Phases of a request as HAR times them (`connect` includes `ssl`; it is drawn apart).
const PHASES: [&str; 8] = [
    "Blocked",
    "DNS",
    "Connect",
    "TLS",
    "Send",
    "Wait",
    "Receive",
    "Not split",
];

/// Milliseconds since 1970 of an ISO 8601 date and time (`2026-10-01T10:00:00.123+02:00`).
fn millis(t: &str) -> Option<f64> {
    let b = t.as_bytes();
    let n = |r: std::ops::Range<usize>| -> Option<i64> {
        let s = t.get(r)?;
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &t[19..];
    let mut frac = 0.0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits = r.bytes().take_while(u8::is_ascii_digit).count();
        frac = format!("0.{}", &r[..digits]).parse().unwrap_or(0.0);
        rest = &r[digits..];
    }
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z') => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let r = rest[1..].replace(':', "");
            let oh: i64 = r.get(0..2)?.parse().ok()?;
            let om: i64 = r.get(2..4).map_or(Some(0), |m| m.parse().ok())?;
            let m = oh * 60 + om;
            if sign == b'+' { m } else { -m }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset * 60;
    Some((secs as f64 + frac) * 1000.0)
}

/// The waterfall row of an entry, its start still absolute.
fn span_of_entry(n: &str, method: &str, url: &str, e: &Node) -> Option<Span> {
    let start = millis(&s(e, "startedDateTime"))?;
    let t = |k: &str| num(&s(e, &format!("timings/{k}"))).unwrap_or(0.0);
    let ssl = t("ssl");
    let mut parts = vec![
        t("blocked"),
        t("dns"),
        (t("connect") - ssl).max(0.0),
        ssl,
        t("send"),
        t("wait"),
        t("receive"),
        0.0,
    ];
    if parts.iter().sum::<f64>() <= 0.0 {
        parts[7] = num(&s(e, "time")).unwrap_or(0.0);
    }
    Some(Span {
        label: format!("#{n} {method} {}", clip(url, 120)),
        start,
        parts,
        source: span(e),
    })
}

fn num(v: &str) -> Option<f64> {
    v.parse::<f64>().ok().filter(|x| x.is_finite() && *x >= 0.0)
}

/// A header value safe to show: credentials of an Authorization header are replaced by
/// their scheme and length.
fn header_value(name: &str, value: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if lower == "authorization" || lower == "proxy-authorization" {
        let scheme = value.split_whitespace().next().unwrap_or("");
        return format!(
            "{scheme} (credentials not shown, {} characters)",
            value.len().saturating_sub(scheme.len()).saturating_sub(1)
        );
    }
    clip(value, 2000)
}

fn custom(root: &Node, out: &mut Inspection) {
    let Some(log) = root.get("log") else {
        return;
    };
    out.fact(
        "Recording",
        "Creator",
        join(&[s(log, "creator/name"), s(log, "creator/version")], " "),
        log.get("creator").and_then(span),
    );
    out.fact(
        "Recording",
        "Browser",
        join(&[s(log, "browser/name"), s(log, "browser/version")], " "),
        log.get("browser").and_then(span),
    );
    out.fact("Recording", "Comment", s(log, "comment"), None);
    let pages = nodes(log, "pages");
    let mut pt = Rows::new(
        "Pages",
        &[
            "ID",
            "Title",
            "Started",
            "Content loaded (ms)",
            "Loaded (ms)",
        ],
    );
    for p in &pages {
        pt.push(
            vec![
                s(p, "id"),
                s(p, "title"),
                s(p, "startedDateTime"),
                known(&s(p, "pageTimings/onContentLoad")),
                known(&s(p, "pageTimings/onLoad")),
            ],
            span(p),
        );
    }
    let entries = nodes(log, "entries");
    let mut et = Rows::new(
        "Entries",
        &[
            "#",
            "Started",
            "Method",
            "URL",
            "Status",
            "Status text",
            "MIME type",
            "Response size (bytes)",
            "Total time (ms)",
            "Server IP",
            "Page",
        ],
    );
    let mut tt = Rows::new(
        "Timings",
        &[
            "#", "URL", "Blocked", "DNS", "Connect", "SSL", "Send", "Wait", "Receive", "Total",
        ],
    );
    let mut statuses: Vec<(String, u64)> = vec![];
    let mut total_bytes = 0f64;
    let mut transferred = 0f64;
    let mut slowest: Option<(f64, String)> = None;
    let mut methods: Vec<(String, u64)> = vec![];
    let mut waterfall: Vec<Span> = vec![];
    for (i, e) in entries.iter().enumerate() {
        let n = (i + 1).to_string();
        let url = s(e, "request/url");
        let method = s(e, "request/method");
        let status = s(e, "response/status");
        let size = known(&s(e, "response/content/size"));
        let body = known(&s(e, "response/bodySize"));
        let shown_size = if size.is_empty() { body.clone() } else { size };
        total_bytes += num(&shown_size).unwrap_or(0.0);
        transferred +=
            num(&body).unwrap_or(0.0) + num(&s(e, "response/headersSize")).unwrap_or(0.0);
        let time = s(e, "time");
        if let Some(t) = num(&time)
            && slowest.as_ref().is_none_or(|(best, _)| t > *best)
        {
            slowest = Some((t, format!("{method} {url}")));
        }
        for (list, key) in [
            (&mut statuses, status.clone()),
            (&mut methods, method.clone()),
        ] {
            match list.iter_mut().find(|(k, _)| *k == key) {
                Some((_, c)) => *c += 1,
                None => list.push((key, 1)),
            }
        }
        et.push(
            vec![
                n.clone(),
                s(e, "startedDateTime"),
                method.clone(),
                url.clone(),
                status.clone(),
                s(e, "response/statusText"),
                s(e, "response/content/mimeType"),
                shown_size,
                time.clone(),
                s(e, "serverIPAddress"),
                s(e, "pageref"),
            ],
            span(e),
        );
        if waterfall.len() < MAX_WATERFALL
            && let Some(row) = span_of_entry(&n, &method, &url, e)
        {
            waterfall.push(row);
        }
        let t = |k: &str| known(&s(e, &format!("timings/{k}")));
        tt.push(
            vec![
                n.clone(),
                clip(&url, 120),
                t("blocked"),
                t("dns"),
                t("connect"),
                t("ssl"),
                t("send"),
                t("wait"),
                t("receive"),
                time,
            ],
            e.get("timings").and_then(span),
        );
        // Headers, cookies and parameters per entry.
        outline(
            out,
            0,
            format!("#{n} {method} {}", clip(&url, 200)),
            join(&[status, s(e, "response/statusText")], " "),
            span(e),
        );
        for (side, label) in [("request", "Request"), ("response", "Response")] {
            let Some(part) = e.get(side) else {
                continue;
            };
            outline(
                out,
                1,
                label,
                join(&[s(part, "httpVersion"), s(part, "content/mimeType")], " "),
                span(part),
            );
            for h in nodes(part, "headers").into_iter().take(500) {
                let name = s(h, "name");
                outline(
                    out,
                    2,
                    name.clone(),
                    header_value(&name, &s(h, "value")),
                    span(h),
                );
            }
            for c in nodes(part, "cookies").into_iter().take(500) {
                let tag = if side == "request" {
                    "Cookie"
                } else {
                    "Set-Cookie"
                };
                outline(
                    out,
                    2,
                    format!("{tag} {}", s(c, "name")),
                    clip(&s(c, "value"), 2000),
                    span(c),
                );
            }
            for q in nodes(part, "queryString").into_iter().take(500) {
                outline(
                    out,
                    2,
                    format!("Query parameter {}", s(q, "name")),
                    clip(&s(q, "value"), 2000),
                    span(q),
                );
            }
            if let Some(post) = part.get("postData") {
                let text = s(post, "text");
                let params: Vec<String> = nodes(post, "params")
                    .into_iter()
                    .take(200)
                    .map(|p| s(p, "name"))
                    .collect();
                outline(
                    out,
                    2,
                    "Posted data",
                    join(
                        &[
                            s(post, "mimeType"),
                            if text.is_empty() {
                                String::new()
                            } else {
                                format!("{} characters (content not shown)", text.chars().count())
                            },
                            if params.is_empty() {
                                String::new()
                            } else {
                                format!("fields: {}", params.join(", "))
                            },
                        ],
                        ", ",
                    ),
                    span(post),
                );
            }
        }
    }
    out.fact("Requests", "Entries", entries.len().to_string(), None);
    out.fact("Requests", "Pages", pages.len().to_string(), None);
    out.fact(
        "Requests",
        "Total response bytes (content)",
        format!("{total_bytes:.0}"),
        None,
    );
    if transferred > 0.0 {
        out.fact(
            "Requests",
            "Transferred bytes (headers and bodies)",
            format!("{transferred:.0}"),
            None,
        );
    }
    if let Some((t, what)) = slowest {
        out.fact(
            "Requests",
            "Slowest request",
            format!("{} — {t:.0} ms", clip(&what, 200)),
            None,
        );
    }
    statuses.sort();
    for (code, c) in &statuses {
        out.fact(
            "Status codes",
            if code.is_empty() || code == "0" {
                "no response (0)"
            } else {
                code
            },
            c.to_string(),
            None,
        );
    }
    for (m, c) in &methods {
        out.fact("Methods", m, c.to_string(), None);
    }
    pt.finish(out);
    et.finish(out);
    tt.finish(out);
    if let Some(first) = waterfall.iter().map(|r| r.start).reduce(f64::min) {
        for r in &mut waterfall {
            r.start -= first;
        }
        if entries.len() > waterfall.len() {
            out.notes.push(format!(
                "The waterfall draws {} of {} requests (those with a readable start, up to {MAX_WATERFALL})",
                waterfall.len(),
                entries.len()
            ));
        }
        out.visuals.push(Visual::Timeline {
            rows: waterfall,
            phases: PHASES.iter().map(|p| (*p).to_owned()).collect(),
            unit: "ms".into(),
        });
    }
    out.notes.push(
        "Nothing is fetched: URLs, cookies and headers are shown as recorded. Authorization credentials and posted data are not shown; cookie values can be masked".into(),
    );
}
