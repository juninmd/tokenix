//! Symbols tab: browse the symbol graph without leaving the shell.
//!
//! Left: symbols ranked by PageRank (type to filter by name or path, `t` cycles
//! the kind). Right: the selected symbol's callers and callees — `Enter` jumps
//! into one, `Backspace` walks back — and its source. Nothing here searches
//! file contents: it only reads `graph_nodes` / `graph_edges`, the part of the
//! index an agent cannot get from grep.

use std::path::PathBuf;

use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::{copy_to_clipboard, list_widget, trunc};
use crate::store::{self, GraphNode};

/// Rows fetched per query; the list is a window onto the ranked graph.
const MAX_NODES: usize = 300;
/// Callers / callees shown per symbol.
const MAX_RELATED: usize = 40;
/// Source lines kept for the preview pane.
const MAX_SNIPPET: usize = 80;

#[derive(Clone, Copy, PartialEq)]
pub enum Focus {
    List,
    Related,
}

#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Caller,
    Callee,
}

struct Related {
    dir: Dir,
    node: GraphNode,
    reference: String,
}

pub struct Explorer {
    repo_root: PathBuf,
    query: String,
    pub searching: bool,
    /// `""` = every kind, then the kinds present in the graph.
    kinds: Vec<String>,
    kind_i: usize,
    nodes: Vec<GraphNode>,
    sel: usize,
    focus: Focus,
    related: Vec<Related>,
    rel_sel: usize,
    snippet: Vec<String>,
    /// `(query, chunk_id)` frames pushed by every jump, popped by Backspace.
    history: Vec<(String, i64)>,
    loaded: bool,
    indexed: bool,
    msg: Option<String>,
}

impl Explorer {
    pub fn new(repo_root: PathBuf) -> Self {
        Explorer {
            repo_root,
            query: String::new(),
            searching: false,
            kinds: vec![String::new()],
            kind_i: 0,
            nodes: Vec::new(),
            sel: 0,
            focus: Focus::List,
            related: Vec::new(),
            rel_sel: 0,
            snippet: Vec::new(),
            history: Vec::new(),
            loaded: false,
            indexed: false,
            msg: None,
        }
    }

    /// Load on first view, so opening another tab never pays for the query.
    pub fn ensure_loaded(&mut self) {
        if !self.loaded {
            self.reload();
        }
    }

    pub fn reload(&mut self) {
        self.loaded = true;
        let conn = match store::open_db(&self.repo_root, false) {
            Ok(Some(c)) => c,
            _ => {
                self.indexed = false;
                self.nodes.clear();
                self.related.clear();
                self.snippet.clear();
                return;
            }
        };
        self.indexed = true;
        if self.kinds.len() == 1 {
            if let Ok(found) = store::graph_node_kinds(&conn) {
                self.kinds.extend(found);
            }
        }
        let kind = self.kind_filter().map(str::to_string);
        self.nodes = store::search_graph_nodes_kind(&conn, &self.query, MAX_NODES, kind.as_deref())
            .unwrap_or_default();
        self.sel = self.sel.min(self.nodes.len().saturating_sub(1));
        self.load_detail(&conn);
    }

    fn kind_filter(&self) -> Option<&str> {
        self.kinds
            .get(self.kind_i)
            .map(String::as_str)
            .filter(|k| !k.is_empty())
    }

    fn current(&self) -> Option<&GraphNode> {
        self.nodes.get(self.sel)
    }

    /// Callers, callees and source of the selected symbol.
    fn load_detail(&mut self, conn: &rusqlite::Connection) {
        self.related.clear();
        self.rel_sel = 0;
        self.snippet.clear();
        let Some(node) = self.current().cloned() else {
            return;
        };
        for (dir, callers) in [(Dir::Caller, true), (Dir::Callee, false)] {
            let rels = store::relations_for_node(conn, node.chunk_id, callers).unwrap_or_default();
            for r in rels.into_iter().take(MAX_RELATED) {
                let (other, reference) = if callers {
                    (r.from, r.reference)
                } else {
                    (r.to, r.reference)
                };
                self.related.push(Related {
                    dir,
                    node: other,
                    reference,
                });
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.repo_root.join(&node.path)) {
            self.snippet = text
                .lines()
                .skip(node.start_line.saturating_sub(1))
                .take(
                    node.end_line
                        .saturating_sub(node.start_line)
                        .saturating_add(1)
                        .min(MAX_SNIPPET),
                )
                .map(str::to_string)
                .collect();
        }
    }

    fn refresh_detail(&mut self) {
        if let Ok(Some(conn)) = store::open_db(&self.repo_root, false) {
            self.load_detail(&conn);
        }
    }

    pub fn key_search(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.query.clear();
                self.searching = false;
            }
            KeyCode::Enter => self.searching = false,
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Char(c) => self.query.push(c),
            _ => return,
        }
        self.sel = 0;
        self.focus = Focus::List;
        self.reload();
    }

    pub fn key(&mut self, code: KeyCode) {
        self.msg = None;
        match code {
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('t') => {
                self.kind_i = (self.kind_i + 1) % self.kinds.len().max(1);
                self.sel = 0;
                self.reload();
            }
            KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::PageDown => self.step(10),
            KeyCode::PageUp => self.step(-10),
            KeyCode::Home | KeyCode::Char('g') => self.step(isize::MIN / 2),
            KeyCode::End => self.step(isize::MAX / 2),
            KeyCode::Enter => match self.focus {
                Focus::List if !self.related.is_empty() => self.focus = Focus::Related,
                Focus::List => {}
                Focus::Related => self.jump(),
            },
            KeyCode::Backspace => self.back(),
            KeyCode::Char('c') => self.copy_location(),
            KeyCode::Char('y') => self.copy_source(),
            _ => {}
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::List if !self.related.is_empty() => Focus::Related,
            _ => Focus::List,
        };
    }

    fn step(&mut self, delta: isize) {
        match self.focus {
            Focus::List => {
                let before = self.sel;
                self.sel = moved(self.sel, delta, self.nodes.len());
                if self.sel != before {
                    self.refresh_detail();
                }
            }
            Focus::Related => self.rel_sel = moved(self.rel_sel, delta, self.related.len()),
        }
    }

    /// Make the highlighted caller/callee the selected symbol.
    fn jump(&mut self) {
        let Some(target) = self.related.get(self.rel_sel).map(|r| r.node.clone()) else {
            return;
        };
        if let Some(cur) = self.current() {
            self.history.push((self.query.clone(), cur.chunk_id));
        }
        self.select_by_id(&target.name, target.chunk_id);
        self.focus = Focus::List;
    }

    fn back(&mut self) {
        match self.history.pop() {
            Some((query, id)) => self.select_by_id(&query, id),
            None => self.focus = Focus::List,
        }
    }

    /// Re-run the query as `query` and land on `chunk_id`, falling back to the
    /// first hit when the kind filter hides it.
    fn select_by_id(&mut self, query: &str, chunk_id: i64) {
        self.query = query.to_string();
        self.kind_i = 0;
        self.reload();
        if let Some(i) = self.nodes.iter().position(|n| n.chunk_id == chunk_id) {
            self.sel = i;
            self.refresh_detail();
        }
        self.focus = Focus::List;
    }

    fn copy_location(&mut self) {
        let loc = match self.focus {
            Focus::Related => self
                .related
                .get(self.rel_sel)
                .map(|r| format!("{}:{}", r.node.path, r.node.start_line)),
            Focus::List => self
                .current()
                .map(|n| format!("{}:{}", n.path, n.start_line)),
        };
        self.msg = Some(match loc {
            Some(l) => match copy_to_clipboard(&l) {
                Ok(()) => format!("copied {l}"),
                Err(e) => format!("copy failed: {e}"),
            },
            None => "nothing selected".to_string(),
        });
    }

    fn copy_source(&mut self) {
        self.msg = Some(if self.snippet.is_empty() {
            "no source to copy".to_string()
        } else {
            match copy_to_clipboard(&self.snippet.join("\n")) {
                Ok(()) => format!("copied {} source lines", self.snippet.len()),
                Err(e) => format!("copy failed: {e}"),
            }
        });
    }

    pub fn footer(&self) -> String {
        if self.searching {
            return format!("/{}_   Enter: apply · Esc: clear", self.query);
        }
        "←→: tab · ↑↓: move · /: filter · t: kind · Tab: callers/callees · Enter: open · ⌫: back · c: copy loc · y: copy src · q".to_string()
    }

    pub fn status(&self) -> Option<&str> {
        self.msg.as_deref()
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        if !self.indexed {
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from("  No index for this repository yet.".yellow()),
                Line::from(""),
                Line::from("  Go to Stats and run “Index repository”, or run `tokenix index`."),
            ])
            .block(Block::bordered().title(" symbols "));
            f.render_widget(p, area);
            return;
        }
        let cols = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);
        self.draw_list(f, cols[0]);
        let right = Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(cols[1]);
        self.draw_related(f, right[0]);
        self.draw_source(f, right[1]);
    }

    fn draw_list(&self, f: &mut Frame, area: Rect) {
        let width = area.width.saturating_sub(4) as usize;
        let items: Vec<ListItem> = self
            .nodes
            .iter()
            .map(|n| {
                let name_w = (width * 6 / 10).max(8);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{:<name_w$}", trunc(&n.name, name_w))),
                    Span::styled(format!(" {}", trunc(&n.kind, 10)), Style::default().dim()),
                ]))
            })
            .collect();
        let kind = self.kind_filter().unwrap_or("all");
        let filter = if self.query.is_empty() {
            String::new()
        } else {
            format!(" · “{}”", self.query)
        };
        let title = format!(
            " symbols · {} · {kind}{filter} ",
            self.nodes.len().min(MAX_NODES)
        );
        let mut state = ListState::default();
        state.select((!self.nodes.is_empty()).then_some(self.sel));
        f.render_stateful_widget(
            list_widget(items, title, self.focus == Focus::List),
            area,
            &mut state,
        );
    }

    fn draw_related(&self, f: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Related;
        let Some(node) = self.current() else {
            let p = Paragraph::new(Line::from("  no symbols match".dim()))
                .block(Block::bordered().title(" relations "));
            f.render_widget(p, area);
            return;
        };
        let callers = self.related.iter().filter(|r| r.dir == Dir::Caller).count();
        let callees = self.related.len() - callers;
        let head = vec![
            Line::from(vec![
                Span::styled(
                    node.name.clone(),
                    Style::default().cyan().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", node.kind), Style::default().dim()),
            ]),
            Line::from(format!(
                "{}:{}-{}",
                node.path, node.start_line, node.end_line
            )),
            Line::from(format!("{callers} caller(s) · {callees} callee(s)").dim()),
        ];
        let inner = Block::bordered().title(" relations ");
        let body = inner.inner(area);
        f.render_widget(inner, area);
        let parts = Layout::vertical([Constraint::Length(head.len() as u16), Constraint::Min(0)])
            .split(body);
        f.render_widget(Paragraph::new(head), parts[0]);
        let items: Vec<ListItem> = self
            .related
            .iter()
            .map(|r| {
                let (arrow, style) = match r.dir {
                    Dir::Caller => ("← ", Style::default().green()),
                    Dir::Callee => ("→ ", Style::default().yellow()),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(arrow, style),
                    Span::raw(r.node.name.clone()),
                    Span::styled(
                        format!("  {}:{}", r.node.path, r.node.start_line),
                        Style::default().dim(),
                    ),
                    Span::styled(
                        if r.reference.is_empty() || r.reference == r.node.name {
                            String::new()
                        } else {
                            format!("  via {}", trunc(&r.reference, 24))
                        },
                        Style::default().dim(),
                    ),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        if focused && !self.related.is_empty() {
            state.select(Some(self.rel_sel));
        }
        let mut list = ratatui::widgets::List::new(items);
        if focused {
            list = list.highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        f.render_stateful_widget(list, parts[1], &mut state);
    }

    fn draw_source(&self, f: &mut Frame, area: Rect) {
        let title = match self.current() {
            Some(n) => format!(" source · {} ", trunc(&n.path, 40)),
            None => " source ".to_string(),
        };
        let start = self.current().map_or(1, |n| n.start_line);
        let lines: Vec<Line> = if self.snippet.is_empty() {
            vec![Line::from("  source not available on disk".dim())]
        } else {
            self.snippet
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    Line::from(vec![
                        Span::styled(format!("{:>5} ", start + i), Style::default().dim()),
                        Span::raw(l.clone()),
                    ])
                })
                .collect()
        };
        f.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(title)),
            area,
        );
    }
}

/// `cur + delta` clamped to `0..len` (an empty list stays at 0).
fn moved(cur: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (cur as isize)
        .saturating_add(delta)
        .clamp(0, len as isize - 1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewChunk;

    /// A repo with `main -> helper -> leaf`, indexed into a throwaway project.
    fn fixture(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("tokenix-explorer-{}-{name}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "fn main() {\n    helper();\n}\nfn helper() {\n    leaf();\n}\nfn leaf() {}\n",
        )
        .unwrap();
        let conn = store::open_db(&root, true).unwrap().unwrap();
        store::init_schema(&conn, 0).unwrap();
        let file = store::upsert_file(&conn, "src/lib.rs", 1.0, "h").unwrap();
        let mut ids = Vec::new();
        for (sym, start, end) in [("main", 1, 3), ("helper", 4, 6), ("leaf", 7, 7)] {
            let id = store::insert_chunk(
                &conn,
                NewChunk {
                    file_id: file,
                    path: "src/lib.rs",
                    start,
                    end,
                    symbol: sym,
                    kind: "function",
                    content: "x",
                    token_count: 1,
                },
            )
            .unwrap();
            store::insert_graph_node(&conn, id, file, "src/lib.rs", sym, "function", start, end)
                .unwrap();
            ids.push(id);
        }
        store::insert_graph_edge(&conn, ids[0], ids[1], "helper", "references").unwrap();
        store::insert_graph_edge(&conn, ids[1], ids[2], "leaf", "references").unwrap();
        root
    }

    fn select(e: &mut Explorer, name: &str) {
        e.sel = e.nodes.iter().position(|n| n.name == name).unwrap();
        e.refresh_detail();
    }

    #[test]
    fn shows_callers_callees_and_source_then_walks_back() {
        let mut e = Explorer::new(fixture("walk"));
        e.reload();
        assert_eq!(e.nodes.len(), 3);

        select(&mut e, "helper");
        let rel: Vec<(bool, &str)> = e
            .related
            .iter()
            .map(|r| (r.dir == Dir::Caller, r.node.name.as_str()))
            .collect();
        assert_eq!(rel, vec![(true, "main"), (false, "leaf")]);
        assert_eq!(e.snippet, vec!["fn helper() {", "    leaf();", "}"]);

        // Enter on the list focuses relations; Enter there opens the callee.
        e.key(KeyCode::Enter);
        assert!(e.focus == Focus::Related);
        e.key(KeyCode::Down);
        e.key(KeyCode::Enter);
        assert_eq!(e.current().unwrap().name, "leaf");
        assert!(e.focus == Focus::List);

        // Backspace returns to where the jump started.
        e.key(KeyCode::Backspace);
        assert_eq!(e.current().unwrap().name, "helper");
    }

    #[test]
    fn filter_narrows_the_list_and_missing_index_is_reported() {
        let mut e = Explorer::new(fixture("filter"));
        e.reload();
        e.key(KeyCode::Char('/'));
        assert!(e.searching);
        for c in "lea".chars() {
            e.key_search(KeyCode::Char(c));
        }
        assert_eq!(e.nodes.len(), 1);
        assert_eq!(e.nodes[0].name, "leaf");
        e.key_search(KeyCode::Esc);
        assert!(!e.searching);
        assert_eq!(e.nodes.len(), 3);

        let mut none = Explorer::new(std::env::temp_dir().join("tokenix-explorer-absent"));
        none.reload();
        assert!(!none.indexed && none.nodes.is_empty());
    }

    #[test]
    fn moved_clamps_to_the_list() {
        assert_eq!(moved(0, -1, 5), 0);
        assert_eq!(moved(3, 10, 5), 4);
        assert_eq!(moved(2, 1, 5), 3);
        assert_eq!(moved(0, isize::MAX / 2, 5), 4);
        assert_eq!(moved(4, isize::MIN / 2, 5), 0);
        assert_eq!(moved(0, 1, 0), 0);
    }
}
