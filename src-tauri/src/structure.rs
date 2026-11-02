//! The structure tree of the open document. Each text revision is parsed on a worker; a newer
//! revision cancels the parse of an older one. The tree stays here and the UI asks for what it
//! shows: children a page at a time, the path to the node under the cursor, diagnostics.
//! Offsets sent to the UI are UTF-16 code units, the editor's unit.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use isolinear_formats::tree::{
    self, Kind, Limits, NodeId, Offsets, Scalar, Severity, Stop, Tree, TreeFormat,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Most children returned by one request.
const CHILDREN_MAX: u32 = 2000;

#[derive(Default)]
pub struct Structure {
    current: Mutex<Option<Arc<Parsed>>>,
    running: Mutex<Option<Arc<AtomicBool>>>,
    next_rev: AtomicU64,
}

struct Parsed {
    rev: u64,
    text: String,
    tree: Tree,
    offsets: Offsets,
    /// Line starts, computed when a label needs a line number.
    lines: OnceLock<Vec<u32>>,
}

impl Parsed {
    fn utf16(&self, byte: u32) -> u32 {
        self.offsets.to_utf16(&self.text, byte)
    }

    fn line_of(&self, byte: u32) -> u32 {
        let lines = self.lines.get_or_init(|| {
            std::iter::once(0)
                .chain(
                    self.text
                        .bytes()
                        .enumerate()
                        .filter(|(_, b)| *b == b'\n')
                        .map(|(i, _)| i as u32 + 1),
                )
                .collect()
        });
        lines.partition_point(|&s| s <= byte) as u32
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Ready {
    rev: u64,
    nodes: u32,
    errors: u32,
    warnings: u32,
    stopped: Option<Stop>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRow {
    id: NodeId,
    kind: Kind,
    scalar: Option<Scalar>,
    name: Option<String>,
    value: Option<String>,
    children: u32,
    /// The line, for kinds whose label is their line (JSON Lines records).
    line: Option<u32>,
    from: u32,
    to: u32,
}

#[derive(Serialize)]
pub struct ChildrenReply {
    rows: Vec<NodeRow>,
    total: u32,
}

#[derive(Serialize)]
pub struct PathStep {
    id: NodeId,
    /// Position among its siblings.
    index: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRow {
    severity: Severity,
    message: String,
    from: u32,
    to: u32,
}

impl Structure {
    /// Parses `text` on a worker and returns its revision number; `structure` is emitted
    /// when the tree is ready.
    pub fn start(&self, app: AppHandle, format: &'static dyn TreeFormat, text: String) -> u64 {
        let rev = self.next_rev.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(old) = self.running.lock().unwrap().replace(cancel.clone()) {
            old.store(true, Ordering::Relaxed);
        }
        let spawned = std::thread::Builder::new()
            .name("structure".into())
            .spawn(move || {
                let tree = format.parse(&text, &Limits::default(), &cancel);
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let offsets = Offsets::new(&text);
                let count = |s: Severity| {
                    tree.diagnostics.iter().filter(|d| d.severity == s).count() as u32
                };
                let ready = Ready {
                    rev,
                    nodes: tree.len() as u32,
                    errors: count(Severity::Error),
                    warnings: count(Severity::Warning),
                    stopped: tree.stopped,
                };
                let parsed = Arc::new(Parsed {
                    rev,
                    text,
                    tree,
                    offsets,
                    lines: OnceLock::new(),
                });
                use tauri::Manager;
                *app.state::<Structure>().current.lock().unwrap() = Some(parsed);
                let _ = app.emit("structure", ready);
            });
        if spawned.is_err() {
            return 0;
        }
        rev
    }

    /// The open document has no tree.
    pub fn clear(&self) {
        if let Some(old) = self.running.lock().unwrap().take() {
            old.store(true, Ordering::Relaxed);
        }
        self.current.lock().unwrap().take();
    }

    fn get(&self, rev: u64) -> Result<Arc<Parsed>, String> {
        match &*self.current.lock().unwrap() {
            Some(p) if p.rev == rev => Ok(p.clone()),
            _ => Err("stale".into()),
        }
    }
}

/// Parses the editor's text after an edit; returns the new revision.
#[tauri::command]
pub fn structure_update(
    app: AppHandle,
    state: tauri::State<'_, Structure>,
    format: String,
    language: Option<String>,
    text: String,
) -> Result<u64, String> {
    let parser = tree::for_format(&format, language.as_deref()).ok_or("this format has no tree")?;
    Ok(state.start(app, parser, text))
}

#[tauri::command]
pub fn tree_children(
    state: tauri::State<'_, Structure>,
    rev: u64,
    node: NodeId,
    start: u32,
    count: u32,
) -> Result<ChildrenReply, String> {
    let p = state.get(rev)?;
    if node as usize >= p.tree.len() {
        return Err("no such node".into());
    }
    let total = p.tree.node(node).child_count();
    let rows = p
        .tree
        .children(node)
        .skip(start as usize)
        .take(count.min(CHILDREN_MAX) as usize)
        .map(|id| row(&p, id))
        .collect();
    Ok(ChildrenReply { rows, total })
}

fn row(p: &Parsed, id: NodeId) -> NodeRow {
    let n = p.tree.node(id);
    let value = n.value.as_ref().or(match n.kind {
        Kind::Text | Kind::Comment | Kind::CData => Some(&n.span),
        _ => None,
    });
    NodeRow {
        id,
        kind: n.kind,
        scalar: n.scalar,
        name: n.name.as_ref().map(|s| tree::preview(&p.text, s)),
        value: value.map(|s| tree::preview(&p.text, s)),
        children: n.child_count(),
        line: (n.kind == Kind::Record).then(|| p.line_of(n.span.start)),
        from: p.utf16(n.span.start),
        to: p.utf16(n.span.end),
    }
}

/// Root to the deepest node at a UTF-16 offset, with each node's position among its siblings.
#[tauri::command]
pub fn tree_path(
    state: tauri::State<'_, Structure>,
    rev: u64,
    offset: u32,
) -> Result<Vec<PathStep>, String> {
    let p = state.get(rev)?;
    let byte = p.offsets.to_byte(&p.text, offset);
    Ok(p.tree
        .path_at(byte)
        .into_iter()
        .map(|id| PathStep {
            id,
            index: p.tree.index_in_parent(id),
        })
        .collect())
}

#[tauri::command]
pub fn tree_diagnostics(
    state: tauri::State<'_, Structure>,
    rev: u64,
) -> Result<Vec<DiagnosticRow>, String> {
    let p = state.get(rev)?;
    let mut rows: Vec<DiagnosticRow> = p
        .tree
        .diagnostics
        .iter()
        .map(|d| DiagnosticRow {
            severity: d.severity,
            message: d.message.clone(),
            from: p.utf16(d.span.start),
            to: p.utf16(d.span.end),
        })
        .collect();
    if p.tree.more_diagnostics > 0 {
        rows.push(DiagnosticRow {
            severity: Severity::Info,
            message: format!("{} more problems are not listed", p.tree.more_diagnostics),
            from: 0,
            to: 0,
        });
    }
    Ok(rows)
}
