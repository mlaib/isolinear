//! JSON Web Tokens (RFC 7519) in compact form: a file holding exactly one `header.payload.
//! signature` of base64url (JWS), or five parts (JWE). The header and claims are decoded and
//! times made readable. Signatures are not verified and JWE content is not decrypted.
use super::util::{base64, bytes_text, clip, printable, span};
use crate::inspect::jsondoc::{self, Node, Value};
use crate::inspect::{Handler, Input, Inspection, Probe, Row, Stage, State, Table};

/// Largest file taken for a token.
const MAX_TOKEN: u64 = 1024 * 1024;

pub struct Jwt;

fn b64url_chars(s: &[u8]) -> bool {
    s.iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// The token's parts with their offsets in the file, when the text is one compact token.
fn parts(b: &[u8]) -> Option<Vec<(usize, &[u8])>> {
    let start = b.iter().position(|c| !c.is_ascii_whitespace())?;
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace())? + 1;
    let token = &b[start..end];
    let mut out = vec![];
    let mut at = start;
    for p in token.split(|&c| c == b'.') {
        if !b64url_chars(p) {
            return None;
        }
        out.push((at, p));
        at += p.len() + 1;
    }
    (out.len() == 3 || out.len() == 5).then_some(out)
}

fn header_json(part: &[u8]) -> Option<Node> {
    let raw = base64(part, true)?;
    let text = std::str::from_utf8(&raw).ok()?;
    let node = jsondoc::parse(text).ok()?;
    matches!(node.value, Value::Object(_)).then_some(node)
}

/// Compact JSON text of a value, bounded.
fn render(n: &Node, depth: usize) -> String {
    match &n.value {
        Value::String(s) => printable(s),
        Value::Array(items) if depth < 4 => {
            let inner: Vec<String> = items
                .iter()
                .take(50)
                .map(|i| render(i, depth + 1))
                .collect();
            if depth == 0 && items.iter().all(Node::is_scalar) {
                inner.join(", ")
            } else {
                format!("[{}]", inner.join(", "))
            }
        }
        Value::Object(fields) if depth < 4 => format!(
            "{{{}}}",
            fields
                .iter()
                .take(50)
                .map(|(k, v)| format!("{}: {}", printable(k), render(v, depth + 1)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => n.text(),
    }
}

fn claim_meaning(key: &str) -> &'static str {
    match key {
        "iss" => "Issuer",
        "sub" => "Subject",
        "aud" => "Audience",
        "exp" => "Expires",
        "nbf" => "Not before",
        "iat" => "Issued at",
        "jti" => "Token ID",
        "name" => "Name",
        "email" => "E-mail",
        "scope" | "scp" => "Scopes",
        "azp" => "Authorized party",
        "nonce" => "Nonce",
        "auth_time" => "Authentication time",
        "sid" => "Session ID",
        "roles" => "Roles",
        "groups" => "Groups",
        "client_id" => "Client ID",
        _ => "",
    }
}

fn header_meaning(key: &str) -> &'static str {
    match key {
        "alg" => "Algorithm",
        "typ" => "Type",
        "kid" => "Key ID",
        "cty" => "Content type",
        "enc" => "Content encryption",
        "zip" => "Compression",
        "jku" => "Key set URL (not fetched)",
        "x5u" => "Certificate URL (not fetched)",
        "x5t" => "Certificate SHA-1 thumbprint",
        "x5t#S256" => "Certificate SHA-256 thumbprint",
        "x5c" => "Certificate chain",
        "crit" => "Critical parameters",
        _ => "",
    }
}

fn time_value(n: &Node) -> Option<(String, i64)> {
    let Value::Number(s) = &n.value else {
        return None;
    };
    let secs: i64 = s.split('.').next()?.parse().ok()?;
    // Years 0..9999 only.
    (-62_135_596_800..=253_402_300_799)
        .contains(&secs)
        .then(|| (crate::binary::utc(secs), secs))
}

fn table_of(
    name: &str,
    node: &Node,
    meaning: fn(&str) -> &'static str,
    src: Option<std::ops::Range<u64>>,
) -> Table {
    let mut t = Table {
        name: name.into(),
        columns: vec!["Name".into(), "Value".into(), "Meaning".into()],
        ..Table::default()
    };
    if let Value::Object(fields) = &node.value {
        for (k, v) in fields.iter().take(crate::inspect::MAX_ROWS) {
            let mut value = render(v, 0);
            if matches!(k.as_str(), "exp" | "nbf" | "iat" | "auth_time")
                && let Some((text, _)) = time_value(v)
            {
                value = format!("{text} ({value})");
            }
            t.rows.push(Row {
                cells: vec![printable(k), clip(&value, 2000), meaning(k).into()],
                source: src.clone(),
            });
        }
    }
    t
}

impl Handler for Jwt {
    fn name(&self) -> &'static str {
        "JSON Web Token"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if probe.size > MAX_TOKEN || probe.size > probe.head.len() as u64 {
            return false;
        }
        let Some(p) = parts(probe.head) else {
            return false;
        };
        header_json(p[0].1).is_some_and(|h| h.get("alg").is_some())
    }
    fn inspect(&self, input: &Input, _cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let p = parts(input.bytes).ok_or("Not a compact JSON Web Token")?;
        let header = header_json(p[0].1).ok_or("The header is not base64url-encoded JSON")?;
        let jwe = p.len() == 5;
        let mut out = Inspection::new(
            "JOSE",
            if jwe {
                "JSON Web Encryption (JWE, compact)"
            } else {
                "JSON Web Token (JWS, compact)"
            },
        );
        let src = |i: usize| span(p[i].0, p[i].1.len());
        let get = |k: &str| header.get(k).map(|v| render(v, 0)).unwrap_or_default();
        out.fact("Header", "Algorithm", get("alg"), src(0));
        out.fact("Header", "Type", get("typ"), src(0));
        out.fact("Header", "Key ID", get("kid"), src(0));
        out.fact("Header", "Content type", get("cty"), src(0));
        out.tables
            .push(table_of("Header", &header, header_meaning, src(0)));
        if jwe {
            out.fact("Header", "Content encryption", get("enc"), src(0));
            for (i, label) in [
                (1, "Encrypted key"),
                (2, "Initialization vector"),
                (3, "Ciphertext"),
                (4, "Authentication tag"),
            ] {
                let len = base64(p[i].1, true).map(|d| d.len()).unwrap_or(0);
                out.fact("Parts", label, bytes_text(len as u64), src(i));
            }
            out.check(
                Stage::Parsed,
                State::Partial,
                "Header decoded; the content is encrypted and was not decrypted",
            );
            out.check(
                Stage::Signature,
                State::NotDone,
                "Not decrypted and not authenticated",
            );
            out.not_validated();
            return Ok(out);
        }
        let payload = base64(p[1].1, true)
            .and_then(|raw| String::from_utf8(raw).ok())
            .and_then(|t| jsondoc::parse(&t).ok());
        let mut parsed = true;
        match &payload {
            Some(claims) if matches!(claims.value, Value::Object(_)) => {
                for (key, label) in [
                    ("iss", "Issuer"),
                    ("sub", "Subject"),
                    ("aud", "Audience"),
                    ("jti", "Token ID"),
                ] {
                    if let Some(v) = claims.get(key) {
                        out.fact("Claims", label, clip(&render(v, 0), 2000), src(1));
                    }
                }
                let mut times = std::collections::HashMap::new();
                for (key, label) in [
                    ("iat", "Issued at"),
                    ("nbf", "Not before"),
                    ("exp", "Expires"),
                ] {
                    if let Some((text, secs)) = claims.get(key).and_then(time_value) {
                        out.fact("Claims", label, text, src(1));
                        times.insert(key, secs);
                    }
                }
                if let (Some(&exp), Some(now)) = (times.get("exp"), super::x509::now()) {
                    let nbf = times.get("nbf").copied().unwrap_or(i64::MIN);
                    let (state, text) = super::x509::validity_status(now, nbf, exp);
                    out.fact(
                        "Claims",
                        "Status",
                        format!("{text} (on this computer's clock)"),
                        None,
                    );
                    out.check(
                        Stage::Semantic,
                        state,
                        format!("Expiry compared with this computer's clock — {text}"),
                    );
                }
                out.tables
                    .push(table_of("Claims", claims, claim_meaning, src(1)));
            }
            Some(_) => {
                out.fact("Payload", "Content", "JSON that is not an object", src(1));
            }
            None => {
                parsed = false;
                let len = base64(p[1].1, true).map(|d| d.len()).unwrap_or(0);
                out.fact(
                    "Payload",
                    "Content",
                    format!("not JSON ({})", bytes_text(len as u64)),
                    src(1),
                );
            }
        }
        let sig_len = base64(p[2].1, true).map(|d| d.len()).unwrap_or(0);
        out.fact("Signature", "Size", bytes_text(sig_len as u64), src(2));
        if get("alg") == "none" {
            out.notes
                .push("The token is unsecured (algorithm \"none\"): anyone can change it.".into());
        }
        out.check(
            Stage::Parsed,
            if parsed {
                State::Passed
            } else {
                State::Partial
            },
            if parsed {
                "Header and claims decoded"
            } else {
                "Header decoded; the payload is not JSON"
            },
        );
        out.not_validated();
        out.check(
            Stage::Signature,
            State::NotDone,
            "Decoded only; the signature was not verified",
        );
        out.notes.push(
            "Key and certificate URLs in the header are shown as text and never fetched.".into(),
        );
        Ok(out)
    }
}
