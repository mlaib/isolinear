//! CycloneDX software bills of materials, in JSON (`bomFormat: CycloneDX`) or XML (namespace
//! `http://cyclonedx.org/schema/bom/1.x`). The XML form is read as its JSON equivalent.
use super::records::{
    Rows, XmlView, all, clip, join, nodes, outline, s, span, str_after, xml_to_node,
};
use crate::inspect::{
    Inspection, Stage, State,
    jsondoc::Node,
    spec::{JsonSpec, XmlSpec},
};
use std::collections::HashMap;

const NS_PREFIX: &str = "http://cyclonedx.org/schema/bom/";

pub static CYCLONEDX_JSON: JsonSpec = JsonSpec {
    family: "CycloneDX",
    kind: "Software bill of materials",
    matches: |keys, head| {
        keys.iter().any(|k| k == "bomFormat")
            && str_after(head, "bomFormat").is_some_and(|v| v == "CycloneDX")
    },
    version: |root| {
        let v = s(root, "specVersion");
        (!v.is_empty()).then(|| format!("CycloneDX {v}"))
    },
    facts: &[],
    tables: &[],
    custom: Some(bom),
};

pub static CYCLONEDX_XML: XmlSpec = XmlSpec {
    family: "CycloneDX",
    kind: "Software bill of materials",
    matches: |root, ns| root == "bom" && ns.starts_with(NS_PREFIX),
    version: |doc| {
        doc.root()
            .ns
            .strip_prefix(NS_PREFIX)
            .map(|v| format!("CycloneDX {v}"))
    },
    facts: &[],
    tables: &[],
    custom: Some(|doc, out| bom(&xml_to_node(doc, 0, XmlView::Plain), out)),
};

fn licenses(c: &Node) -> String {
    let mut parts = vec![];
    for path in [
        "licenses/license/id",
        "licenses/license/name",
        "licenses/id",
        "licenses/name",
        "licenses/expression",
    ] {
        let v = all(c, path, ", ");
        if !v.is_empty() {
            parts.push(v);
        }
    }
    join(&parts, ", ")
}

fn component_line(c: &Node) -> String {
    join(
        &[
            s(c, "type"),
            join(&[s(c, "group"), s(c, "name")], "/"),
            s(c, "version"),
        ],
        " ",
    )
}

/// Components, nested ones after their parent (bounded depth).
fn components<'a>(n: &'a Node, depth: usize, out: &mut Vec<(&'a Node, usize)>) {
    if depth > 16 || out.len() > 1_000_000 {
        return;
    }
    for c in nodes(n, "components") {
        out.push((c, depth));
        components(c, depth + 1, out);
    }
}

fn bom(root: &Node, out: &mut Inspection) {
    let mut spec = s(root, "specVersion");
    if spec.is_empty() {
        let v = out.version.clone().unwrap_or_default();
        spec = v.trim_start_matches("CycloneDX ").to_owned();
    }
    out.fact("BOM", "Spec version", spec, None);
    for (label, path) in [
        ("Serial number", "serialNumber"),
        ("BOM version", "version"),
        ("Timestamp", "metadata/timestamp"),
        ("Lifecycle", "metadata/lifecycles/phase"),
    ] {
        if let Some(n) = nodes(root, path).first() {
            out.fact("BOM", label, n.text(), span(n));
        }
    }
    if let Some(m) = nodes(root, "metadata/component").first() {
        out.fact("Subject component", "Component", component_line(m), span(m));
        out.fact("Subject component", "bom-ref", s(m, "bom-ref"), None);
        out.fact("Subject component", "Package URL", s(m, "purl"), None);
        out.fact("Subject component", "Licenses", licenses(m), None);
    }
    out.fact("BOM", "Supplier", s(root, "metadata/supplier/name"), None);
    out.fact(
        "BOM",
        "Manufacturer",
        s(
            root,
            "metadata/manufacture/name | metadata/manufacturer/name",
        ),
        None,
    );
    out.fact(
        "BOM",
        "BOM licenses",
        licenses(root.get("metadata").unwrap_or(root)),
        None,
    );
    // Tools: a list (1.4 and older) or components and services (1.5+).
    let mut tools: Vec<String> = vec![];
    for t in nodes(root, "metadata/tools")
        .into_iter()
        .chain(nodes(root, "metadata/tools/components"))
        .chain(nodes(root, "metadata/tools/services"))
        .chain(nodes(root, "metadata/tools/tool"))
    {
        let name = s(t, "name");
        if name.is_empty() {
            continue;
        }
        let line = join(
            &[s(t, "vendor | group | publisher"), name, s(t, "version")],
            " ",
        );
        if !tools.contains(&line) {
            tools.push(line);
        }
    }
    out.fact("BOM", "Tools", tools.join("; "), None);

    // Components.
    let mut all_components = vec![];
    components(root, 0, &mut all_components);
    let mut names: HashMap<String, String> = HashMap::new();
    if let Some(m) = nodes(root, "metadata/component").first() {
        names.insert(s(m, "bom-ref"), component_line(m));
    }
    let mut table = Rows::new(
        "Components",
        &[
            "Type",
            "Name",
            "Version",
            "Package URL",
            "Group",
            "Licenses",
            "Hashes",
            "Scope",
            "bom-ref",
            "Nested in",
        ],
    );
    let mut types: Vec<(String, u64)> = vec![];
    let mut parent_stack: Vec<String> = vec![];
    for (c, depth) in &all_components {
        parent_stack.truncate(*depth);
        let name = s(c, "name");
        let ty = s(c, "type");
        match types.iter_mut().find(|(t, _)| *t == ty) {
            Some((_, n)) => *n += 1,
            None => types.push((ty.clone(), 1)),
        }
        let r = s(c, "bom-ref");
        if !r.is_empty() && names.len() < 1_000_000 {
            names.insert(r.clone(), component_line(c));
        }
        table.push(
            vec![
                ty,
                name.clone(),
                s(c, "version"),
                s(c, "purl"),
                s(c, "group"),
                licenses(c),
                {
                    let h = nodes(c, "hashes").len();
                    if h > 0 { h.to_string() } else { String::new() }
                },
                s(c, "scope"),
                r,
                parent_stack.last().cloned().unwrap_or_default(),
            ],
            span(c),
        );
        parent_stack.push(name);
    }
    out.fact("Components", "Components", table.seen().to_string(), None);
    for (t, n) in &types {
        out.fact(
            "Components by type",
            if t.is_empty() { "(no type)" } else { t },
            n.to_string(),
            None,
        );
    }
    table.finish(out);

    // Dependencies.
    let deps = nodes(root, "dependencies");
    let (mut known, mut unknown) = (0u64, 0u64);
    let mut unknown_examples: Vec<String> = vec![];
    let mut check_ref = |r: &str, known: &mut u64, unknown: &mut u64| {
        if names.contains_key(r) {
            *known += 1;
        } else {
            *unknown += 1;
            if unknown_examples.len() < 5 {
                unknown_examples.push(r.to_owned());
            }
        }
    };
    let name_of = |r: &str| names.get(r).cloned().unwrap_or_default();
    let mut edges = 0usize;
    let mut dep_lines = vec![];
    for d in &deps {
        let from = s(d, "ref");
        let mut to: Vec<String> = nodes(d, "dependsOn").iter().map(|n| n.text()).collect();
        to.extend(nodes(d, "dependency/ref").iter().map(|n| n.text()));
        to.extend(
            nodes(d, "dependency")
                .iter()
                .filter(|n| n.is_scalar())
                .map(|n| n.text()),
        );
        edges += to.len();
        dep_lines.push((from, to, span(d)));
    }
    for (from, to, src) in dep_lines.iter().take(100_000) {
        check_ref(from, &mut known, &mut unknown);
        outline(
            out,
            0,
            from.clone(),
            join(
                &[
                    name_of(from),
                    if to.is_empty() {
                        "no dependencies".into()
                    } else {
                        format!("depends on {}", to.len())
                    },
                ],
                " · ",
            ),
            src.clone(),
        );
        for t in to.iter().take(10_000) {
            check_ref(t, &mut known, &mut unknown);
            outline(out, 1, t.clone(), name_of(t), None);
        }
    }
    if !deps.is_empty() {
        out.fact(
            "Dependencies",
            "Dependency entries",
            deps.len().to_string(),
            None,
        );
        out.fact("Dependencies", "Edges", edges.to_string(), None);
    }

    // Vulnerabilities.
    let vulns = nodes(root, "vulnerabilities");
    let mut vt = Rows::new(
        "Vulnerabilities",
        &[
            "ID",
            "Source",
            "Severity",
            "Score",
            "Method",
            "Affects",
            "Analysis",
            "Published",
            "Description",
        ],
    );
    let mut severities: Vec<(String, u64)> = vec![];
    for v in &vulns {
        let ratings = nodes(v, "ratings");
        let sev: Vec<String> = ratings.iter().map(|r| s(r, "severity")).collect();
        let score: Vec<String> = ratings.iter().map(|r| s(r, "score")).collect();
        let method: Vec<String> = ratings.iter().map(|r| s(r, "method")).collect();
        let top = sev
            .iter()
            .find(|s| !s.is_empty())
            .cloned()
            .unwrap_or_default();
        match severities.iter_mut().find(|(t, _)| *t == top) {
            Some((_, n)) => *n += 1,
            None => severities.push((top.clone(), 1)),
        }
        let mut affects: Vec<String> = nodes(v, "affects/ref").iter().map(|n| n.text()).collect();
        affects.extend(nodes(v, "affects/target/ref").iter().map(|n| n.text()));
        for a in &affects {
            check_ref(a, &mut known, &mut unknown);
        }
        vt.push(
            vec![
                s(v, "id"),
                s(v, "source/name"),
                join(&sev, ", "),
                join(&score, ", "),
                join(&method, ", "),
                affects
                    .iter()
                    .map(|a| {
                        let n = name_of(a);
                        if n.is_empty() {
                            a.clone()
                        } else {
                            format!("{a} ({n})")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
                s(v, "analysis/state"),
                s(v, "published"),
                clip(&s(v, "description | detail"), 200),
            ],
            span(v),
        );
    }
    if !vulns.is_empty() {
        out.fact(
            "Vulnerabilities",
            "Vulnerabilities",
            vulns.len().to_string(),
            None,
        );
        for (sev, n) in &severities {
            out.fact(
                "Vulnerabilities",
                &format!("Severity {}", if sev.is_empty() { "unrated" } else { sev }),
                n.to_string(),
                None,
            );
        }
    }
    vt.finish(out);

    // Services.
    let mut st = Rows::new(
        "Services",
        &[
            "Name",
            "Version",
            "Provider",
            "Endpoints (not contacted)",
            "Authenticated",
            "Crosses trust boundary",
            "bom-ref",
        ],
    );
    for sv in nodes(root, "services") {
        st.push(
            vec![
                s(sv, "name"),
                s(sv, "version"),
                s(sv, "provider/name"),
                all(sv, "endpoints", ", "),
                s(sv, "authenticated"),
                s(sv, "x-trust-boundary | trustZone"),
                s(sv, "bom-ref"),
            ],
            span(sv),
        );
    }
    st.finish(out);

    if known + unknown > 0 {
        out.check(
            Stage::Semantic,
            if unknown > 0 { State::Failed } else { State::Passed },
            format!(
                "References to components (dependencies, affected components): {known} match a bom-ref, {unknown} do not{}",
                if unknown_examples.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", unknown_examples.join(", "))
                }
            ),
        );
    }
    out.notes.push(
        "Package URLs, hashes and licences are shown as declared; nothing is downloaded or compared with the real packages".into(),
    );
}
