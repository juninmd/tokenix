//! Unified interactive shell for tokenix's human commands, opened by a bare
//! `tokenix` (or `tokenix filter`) in a TTY. One persistent window with a command
//! tab bar on top — move between tabs with ← / → (or Tab) and never re-type a
//! command. It opens on the Stats dashboard (wordmark, version, hook status, index
//! summary). Report commands (Doctor, Gain, Tokenmap) self-execute the binary and
//! show their output in a scrollable pane; Index runs in the foreground with live
//! progress and returns here; Filters is a three-pane browser (groups · filters ·
//! live input→output preview) so nothing needs a deeper drill level.

use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::filters::{self, ActiveFilter};

#[derive(Clone, Copy, PartialEq)]
enum Cmd {
    Stats,
    Filters,
    Gain,
    Doctor,
    Tokenmap,
    Secrets,
    Egress,
}

impl Cmd {
    const ALL: [Cmd; 7] = [
        Cmd::Stats,
        Cmd::Filters,
        Cmd::Gain,
        Cmd::Doctor,
        Cmd::Tokenmap,
        Cmd::Secrets,
        Cmd::Egress,
    ];

    fn index(self) -> usize {
        Cmd::ALL.iter().position(|&c| c == self).unwrap_or(0)
    }

    fn title(self) -> &'static str {
        match self {
            Cmd::Stats => "Stats",
            Cmd::Filters => "Filters",
            Cmd::Gain => "Gain",
            Cmd::Doctor => "Doctor",
            Cmd::Tokenmap => "Tokenmap",
            Cmd::Secrets => "Secrets",
            Cmd::Egress => "Egress",
        }
    }

    /// Report commands render captured output from self-executing the binary.
    fn argv(self) -> Option<&'static [&'static str]> {
        match self {
            Cmd::Doctor => Some(&["doctor"]),
            Cmd::Tokenmap => Some(&["tokenmap"]),
            _ => None,
        }
    }
}

/// A named bucket of filters (a tool prefix, or a source).
struct Group {
    label: String,
    idxs: Vec<usize>,
}

struct AgentStatus {
    name: &'static str,
    installed: bool,
    scope: String,
}

/// Cached gain data for the Gain tab (project or all-projects aggregate).
struct GainView {
    stats: crate::gain::GainStats,
    projects: Vec<(String, i64, usize, usize)>,
}

#[derive(Clone, Copy, PartialEq)]
enum GroupMode {
    Tool,
    Source,
}

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Groups,
    Filters,
}

#[derive(Clone, Copy, PartialEq)]
enum SecGroup {
    Rule,
    Agent,
}

type SecPayload = (Vec<crate::secrets_scan::ScanFinding>, Vec<(String, usize)>);
type EgrPayload = (Vec<crate::egress_scan::EgressFinding>, Vec<(String, usize)>);

#[derive(Clone, Copy, PartialEq)]
enum EgrGroup {
    Host,
    Rule,
    Agent,
    File,
}

struct Shell {
    cmd: Cmd,
    // Filters page ---------------------------------------------------------
    filters: Vec<ActiveFilter>,
    tool_groups: Vec<Group>,
    source_groups: Vec<Group>,
    gmode: GroupMode,
    pane: Pane,
    g_sel: usize,
    sel_group: usize,
    f_sel: usize,
    search: String,
    searching: bool,
    samples: HashMap<String, String>,
    // Dashboard ------------------------------------------------------------
    agents: Vec<AgentStatus>,
    index_stats: Option<(i64, i64, i64)>,
    /// Selected action on the Stats dashboard (0 = index, 1 = install hooks,
    /// 2 = install binary on PATH).
    stats_sel: usize,
    /// Install is armed and awaiting an Enter confirm.
    stats_confirm: bool,
    stats_msg: Option<String>,
    // Gain page ------------------------------------------------------------
    gain_cache: Option<GainView>,
    gain_cost: bool,
    gain_global: bool,
    // Secrets page ---------------------------------------------------------
    secrets: Option<Vec<crate::secrets_scan::ScanFinding>>,
    secrets_counts: Vec<(String, usize)>,
    secrets_rx: Option<Receiver<SecPayload>>,
    sec_groups: Vec<(String, Vec<usize>)>,
    sec_gmode: SecGroup,
    sec_pane: Pane,
    sec_g: usize,
    sec_sel_group: usize,
    sec_i: usize,
    sec_reveal: bool,
    sec_confirm: bool,
    sec_msg: Option<String>,
    spinner: usize,
    // Egress page ---------------------------------------------------------
    egress: Option<Vec<crate::egress_scan::EgressFinding>>,
    egress_counts: Vec<(String, usize)>,
    egress_rx: Option<Receiver<EgrPayload>>,
    egr_groups: Vec<(String, Vec<usize>)>,
    egr_gmode: EgrGroup,
    egr_pane: Pane,
    egr_reputation: crate::egress_scan::HostReputation,
    egr_g: usize,
    egr_sel_group: usize,
    egr_i: usize,
    // Report / install pages ----------------------------------------------
    reports: HashMap<usize, String>,
    scroll: u16,
    request_index: bool,
}

/// Open the shell on the Stats dashboard. Used by bare `tokenix` and `tokenix filter`.
pub fn run() -> Result<()> {
    let filters = filters::load_active_filters();
    let tool_groups = group_by(&filters, |f| tool_of(&f.name).to_string());
    let source_groups = group_by(&filters, |f| f.source.to_string());
    let mut shell = Shell {
        cmd: Cmd::Stats,
        filters,
        tool_groups,
        source_groups,
        gmode: GroupMode::Tool,
        pane: Pane::Groups,
        g_sel: 0,
        sel_group: 0,
        f_sel: 0,
        search: String::new(),
        searching: false,
        samples: filters::sample_inputs(),
        agents: detect_agents(),
        index_stats: read_index_stats(),
        stats_sel: 0,
        stats_confirm: false,
        stats_msg: None,
        gain_cache: None,
        gain_cost: false,
        gain_global: false,
        secrets: None,
        secrets_counts: Vec::new(),
        secrets_rx: None,
        sec_groups: Vec::new(),
        sec_gmode: SecGroup::Rule,
        sec_pane: Pane::Groups,
        sec_g: 0,
        sec_sel_group: 0,
        sec_i: 0,
        sec_reveal: false,
        sec_confirm: false,
        sec_msg: None,
        spinner: 0,
        egress: None,
        egress_counts: Vec::new(),
        egress_rx: None,
        egr_groups: Vec::new(),
        egr_gmode: EgrGroup::Host,
        egr_pane: Pane::Groups,
        egr_reputation: crate::egress_scan::HostReputation::load(),
        egr_g: 0,
        egr_sel_group: 0,
        egr_i: 0,
        reports: HashMap::new(),
        scroll: 0,
        request_index: false,
    };
    let mut terminal = ratatui::init();
    let res = shell.event_loop(&mut terminal);
    ratatui::restore();
    res
}

impl Shell {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            self.ensure_report();
            self.ensure_gain();
            self.ensure_secrets();
            self.ensure_egress();

            // Collect a finished background secret scan, if any.
            let done = self.secrets_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            if let Some((findings, counts)) = done {
                self.secrets = Some(findings);
                self.secrets_counts = counts;
                self.secrets_rx = None;
                self.rebuild_sec_groups();
            }

            // Collect a finished background egress scan, if any.
            let done = self.egress_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            if let Some((findings, counts)) = done {
                self.egress = Some(findings);
                self.egress_counts = counts;
                self.egress_rx = None;
                self.rebuild_egr_groups();
            }

            terminal.draw(|f| self.draw(f))?;

            // While a scan runs, poll so the spinner animates; otherwise block.
            let scanning = self.secrets_rx.is_some() || self.egress_rx.is_some();
            let timeout = if scanning {
                Duration::from_millis(120)
            } else {
                Duration::from_secs(3600)
            };
            if !event::poll(timeout)? {
                if scanning {
                    self.spinner = self.spinner.wrapping_add(1);
                }
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Search capture (Filters tab) owns all keys while active.
            if self.searching {
                match key.code {
                    KeyCode::Esc => {
                        self.search.clear();
                        self.searching = false;
                    }
                    KeyCode::Enter => self.searching = false,
                    KeyCode::Backspace => {
                        self.search.pop();
                    }
                    KeyCode::Char(c) => self.search.push(c),
                    _ => {}
                }
                self.pane = Pane::Filters;
                self.clamp();
                continue;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Left => self.switch_tab(-1),
                KeyCode::Right => self.switch_tab(1),
                // Tab toggles the two-pane Filters/Secrets focus; elsewhere it
                // advances tabs.
                KeyCode::Tab | KeyCode::BackTab if self.cmd == Cmd::Filters => self.toggle_pane(),
                KeyCode::Tab | KeyCode::BackTab if self.cmd == Cmd::Secrets => {
                    self.sec_pane = other_pane(self.sec_pane)
                }
                KeyCode::Tab | KeyCode::BackTab if self.cmd == Cmd::Egress => {
                    self.egr_pane = other_pane(self.egr_pane)
                }
                KeyCode::Tab => self.switch_tab(1),
                KeyCode::BackTab => self.switch_tab(-1),
                _ => match self.cmd {
                    Cmd::Stats => self.key_stats(key.code),
                    Cmd::Filters => self.key_filters(key.code),
                    Cmd::Gain => self.key_gain(key.code),
                    Cmd::Secrets => self.key_secrets(key.code),
                    Cmd::Egress => self.key_egress(key.code),
                    _ => self.key_scroll(key.code),
                },
            }

            // Index runs in the foreground with live progress: drop out of the
            // alt screen, run it, wait for a keypress, then resume the TUI.
            if self.request_index {
                self.request_index = false;
                ratatui::restore();
                run_index_foreground();
                *terminal = ratatui::init();
                self.index_stats = read_index_stats();
                self.stats_msg = Some("Index updated.".to_string());
            }
        }
    }

    fn switch_tab(&mut self, dir: i32) {
        let cur = self.cmd.index() as i32;
        let next = (cur + dir).rem_euclid(Cmd::ALL.len() as i32) as usize;
        self.cmd = Cmd::ALL[next];
        self.scroll = 0;
        self.searching = false;
        self.search.clear();
        self.stats_confirm = false;
    }

    fn toggle_pane(&mut self) {
        self.pane = match self.pane {
            Pane::Groups => Pane::Filters,
            Pane::Filters => Pane::Groups,
        };
    }

    /// Lazily capture report output the first time its tab is shown.
    fn ensure_report(&mut self) {
        let Some(argv) = self.cmd.argv() else {
            return;
        };
        let idx = self.cmd.index();
        self.reports.entry(idx).or_insert_with(|| capture(argv));
    }

    /// Compute gain data the first time the Gain tab is shown (or after a toggle
    /// that changed the project scope cleared the cache).
    fn ensure_gain(&mut self) {
        if self.cmd != Cmd::Gain || self.gain_cache.is_some() {
            return;
        }
        let view = if self.gain_global {
            let g = crate::gain::compute_global_gain();
            GainView {
                stats: g.aggregate,
                projects: g.projects,
            }
        } else {
            let cwd = std::env::current_dir().unwrap_or_default();
            let root = crate::store::find_project_root(&cwd);
            GainView {
                stats: crate::gain::compute_gain(&root),
                projects: Vec::new(),
            }
        };
        self.gain_cache = Some(view);
    }

    fn key_gain(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('c') => self.gain_cost = !self.gain_cost,
            KeyCode::Char('a') => {
                self.gain_global = !self.gain_global;
                self.gain_cache = None;
                self.scroll = 0;
            }
            KeyCode::Char('r') => {
                self.gain_cache = None;
                self.scroll = 0;
            }
            _ => self.key_scroll(code),
        }
    }

    // ── Secrets tab ──────────────────────────────────────────────────────

    /// Kick off a background scan the first time the Secrets tab is shown (or
    /// after `r` cleared the result). The scan reads SQLite/JSON on a worker
    /// thread so the UI never blocks.
    fn ensure_secrets(&mut self) {
        if self.cmd != Cmd::Secrets || self.secrets.is_some() || self.secrets_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::secrets_scan::scan_findings());
        });
        self.secrets_rx = Some(rx);
    }

    /// Bucket findings by the active grouping (credential rule or agent), sorted
    /// by count desc, and reset the cursors.
    fn rebuild_sec_groups(&mut self) {
        self.sec_g = 0;
        self.sec_sel_group = 0;
        self.sec_i = 0;
        self.sec_groups.clear();
        let Some(findings) = &self.secrets else {
            return;
        };
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, fnd) in findings.iter().enumerate() {
            let key = match self.sec_gmode {
                SecGroup::Rule => fnd.rule.clone(),
                SecGroup::Agent => fnd.agent.clone(),
            };
            if !map.contains_key(&key) {
                order.push(key.clone());
            }
            map.entry(key).or_default().push(i);
        }
        let mut groups: Vec<(String, Vec<usize>)> = order
            .into_iter()
            .map(|k| {
                let v = map.remove(&k).unwrap_or_default();
                (k, v)
            })
            .collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
        self.sec_groups = groups;
    }

    fn sec_items(&self) -> &[usize] {
        self.sec_groups
            .get(self.sec_sel_group)
            .map(|g| g.1.as_slice())
            .unwrap_or(&[])
    }

    /// Distinct secret values within the selected group — each entry is the list
    /// of finding indices sharing that exact value, sorted by occurrence count.
    fn distinct_secrets(&self) -> Vec<Vec<usize>> {
        let Some(findings) = &self.secrets else {
            return Vec::new();
        };
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for &i in self.sec_items() {
            let key = findings[i].secret.clone();
            if !map.contains_key(&key) {
                order.push(key.clone());
            }
            map.entry(key).or_default().push(i);
        }
        let mut out: Vec<Vec<usize>> = order
            .into_iter()
            .map(|k| map.remove(&k).unwrap_or_default())
            .collect();
        out.sort_by_key(|v| std::cmp::Reverse(v.len()));
        out
    }

    /// Finding indices for the highlighted distinct secret (all its occurrences).
    fn current_occurrences(&self) -> Vec<usize> {
        self.distinct_secrets()
            .get(self.sec_i)
            .cloned()
            .unwrap_or_default()
    }

    fn key_secrets(&mut self, code: KeyCode) {
        // A pending [REDACTED] write is armed; the next key confirms or cancels.
        if self.sec_confirm {
            if matches!(code, KeyCode::Char('x') | KeyCode::Char('y')) {
                self.do_redact();
            }
            self.sec_confirm = false;
            return;
        }
        self.sec_msg = None;
        match code {
            KeyCode::Char('s') => {
                self.sec_gmode = match self.sec_gmode {
                    SecGroup::Rule => SecGroup::Agent,
                    SecGroup::Agent => SecGroup::Rule,
                };
                self.rebuild_sec_groups();
            }
            KeyCode::Char('v') => self.sec_reveal = !self.sec_reveal,
            KeyCode::Char('c') if !self.current_occurrences().is_empty() => self.do_copy(),
            KeyCode::Char('x') if !self.current_occurrences().is_empty() => self.sec_confirm = true,
            KeyCode::Char('r') => {
                self.secrets = None;
                self.secrets_rx = None;
                self.sec_groups.clear();
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sec(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sec(-1),
            KeyCode::PageDown => self.move_sec(10),
            KeyCode::PageUp => self.move_sec(-10),
            KeyCode::Home | KeyCode::Char('g') => self.jump_sec(0),
            KeyCode::End | KeyCode::Char('G') => self.jump_sec(usize::MAX),
            _ => {}
        }
    }

    /// Copy the highlighted secret value to the system clipboard so it can be
    /// pasted into the provider console for rotation/revocation without
    /// re-typing it from the screen.
    fn do_copy(&mut self) {
        let occ = self.current_occurrences();
        let secret = {
            let Some(findings) = &self.secrets else {
                return;
            };
            let Some(&rep) = occ.first() else {
                return;
            };
            findings[rep].secret.clone()
        };
        self.sec_msg = Some(match copy_to_clipboard(&secret) {
            Ok(()) => {
                "Secret copied to clipboard — rotate it, then clear the clipboard".to_string()
            }
            Err(e) => format!("Copy failed: {e}"),
        });
    }

    /// Write `[REDACTED]` over the highlighted secret across its files, then
    /// re-scan so the view reflects the edit.
    fn do_redact(&mut self) {
        let occ = self.current_occurrences();
        let (secret, paths) = {
            let Some(findings) = &self.secrets else {
                return;
            };
            let Some(&rep) = occ.first() else {
                return;
            };
            let secret = findings[rep].secret.clone();
            let paths: Vec<std::path::PathBuf> =
                occ.iter().map(|&i| findings[i].path.clone()).collect();
            (secret, paths)
        };
        let (edited, skipped) = crate::secrets_scan::redact_in_files(&secret, &paths);
        let mut msg = format!("Redacted in {edited} file(s)");
        if skipped > 0 {
            msg.push_str(&format!(
                " · {skipped} skipped (database — rotate the credential)"
            ));
        }
        self.sec_msg = Some(msg);
        self.secrets = None;
        self.secrets_rx = None;
        self.sec_groups.clear();
    }

    fn move_sec(&mut self, dir: i32) {
        match self.sec_pane {
            Pane::Groups => {
                let len = self.sec_groups.len();
                if len == 0 {
                    return;
                }
                self.sec_g = (self.sec_g as i32 + dir).rem_euclid(len as i32) as usize;
                self.sec_sel_group = self.sec_g;
                self.sec_i = 0;
            }
            Pane::Filters => {
                let len = self.distinct_secrets().len();
                if len == 0 {
                    return;
                }
                self.sec_i = (self.sec_i as i32 + dir).rem_euclid(len as i32) as usize;
            }
        }
    }

    fn jump_sec(&mut self, target: usize) {
        match self.sec_pane {
            Pane::Groups => {
                let len = self.sec_groups.len();
                if len > 0 {
                    self.sec_g = target.min(len - 1);
                    self.sec_sel_group = self.sec_g;
                    self.sec_i = 0;
                }
            }
            Pane::Filters => {
                let len = self.distinct_secrets().len();
                if len > 0 {
                    self.sec_i = target.min(len - 1);
                }
            }
        }
    }

    // ── Egress tab ───────────────────────────────────────────────────────

    fn ensure_egress(&mut self) {
        if self.cmd != Cmd::Egress || self.egress.is_some() || self.egress_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::egress_scan::scan_findings());
        });
        self.egress_rx = Some(rx);
    }

    fn rebuild_egr_groups(&mut self) {
        self.egr_g = 0;
        self.egr_sel_group = 0;
        self.egr_i = 0;
        self.egr_groups.clear();
        let Some(findings) = &self.egress else {
            return;
        };
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, fnd) in findings.iter().enumerate() {
            let key = match self.egr_gmode {
                EgrGroup::Host => fnd.host.clone(),
                EgrGroup::Rule => fnd.rule.clone(),
                EgrGroup::Agent => fnd.agent.clone(),
                EgrGroup::File => fnd.file.clone(),
            };
            if !map.contains_key(&key) {
                order.push(key.clone());
            }
            map.entry(key).or_default().push(i);
        }
        let mut groups: Vec<(String, Vec<usize>)> = order
            .into_iter()
            .map(|k| {
                let v = map.remove(&k).unwrap_or_default();
                (k, v)
            })
            .collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
        self.egr_groups = groups;
    }

    fn key_egress(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('s') => {
                self.egr_gmode = match self.egr_gmode {
                    EgrGroup::Host => EgrGroup::Rule,
                    EgrGroup::Rule => EgrGroup::Agent,
                    EgrGroup::Agent => EgrGroup::File,
                    EgrGroup::File => EgrGroup::Host,
                };
                self.rebuild_egr_groups();
            }
            KeyCode::Char('r') => {
                self.egress = None;
                self.egress_rx = None;
                self.egr_groups.clear();
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_egr(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_egr(-1),
            KeyCode::PageDown => self.move_egr(10),
            KeyCode::PageUp => self.move_egr(-10),
            KeyCode::Home | KeyCode::Char('g') => self.jump_egr(0),
            KeyCode::End | KeyCode::Char('G') => self.jump_egr(usize::MAX),
            _ => {}
        }
    }

    fn egr_items(&self) -> &[usize] {
        self.egr_groups
            .get(self.egr_sel_group)
            .map(|g| g.1.as_slice())
            .unwrap_or(&[])
    }

    fn distinct_egress_targets(&self) -> Vec<Vec<usize>> {
        let Some(findings) = &self.egress else {
            return Vec::new();
        };
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for &i in self.egr_items() {
            let key = format!("{}\n{}", findings[i].host, findings[i].target);
            if !map.contains_key(&key) {
                order.push(key.clone());
            }
            map.entry(key).or_default().push(i);
        }
        let mut out: Vec<Vec<usize>> = order
            .into_iter()
            .map(|k| map.remove(&k).unwrap_or_default())
            .collect();
        out.sort_by_key(|v| std::cmp::Reverse(v.len()));
        out
    }

    fn current_egress_occurrences(&self) -> Vec<usize> {
        self.distinct_egress_targets()
            .get(self.egr_i)
            .cloned()
            .unwrap_or_default()
    }

    fn move_egr(&mut self, dir: i32) {
        match self.egr_pane {
            Pane::Groups => {
                let len = self.egr_groups.len();
                if len == 0 {
                    return;
                }
                self.egr_g = (self.egr_g as i32 + dir).rem_euclid(len as i32) as usize;
                self.egr_sel_group = self.egr_g;
                self.egr_i = 0;
            }
            Pane::Filters => {
                let len = self.distinct_egress_targets().len();
                if len == 0 {
                    return;
                }
                self.egr_i = (self.egr_i as i32 + dir).rem_euclid(len as i32) as usize;
            }
        }
    }

    fn jump_egr(&mut self, target: usize) {
        match self.egr_pane {
            Pane::Groups => {
                let len = self.egr_groups.len();
                if len > 0 {
                    self.egr_g = target.min(len - 1);
                    self.egr_sel_group = self.egr_g;
                    self.egr_i = 0;
                }
            }
            Pane::Filters => {
                let len = self.distinct_egress_targets().len();
                if len > 0 {
                    self.egr_i = target.min(len - 1);
                }
            }
        }
    }

    // ── Filters tab ──────────────────────────────────────────────────────

    fn groups(&self) -> &[Group] {
        match self.gmode {
            GroupMode::Tool => &self.tool_groups,
            GroupMode::Source => &self.source_groups,
        }
    }

    fn visible_filters(&self) -> Vec<usize> {
        let q = self.search.to_lowercase();
        let Some(group) = self.groups().get(self.sel_group) else {
            return Vec::new();
        };
        group
            .idxs
            .iter()
            .copied()
            .filter(|&fi| q.is_empty() || self.filters[fi].name.to_lowercase().contains(&q))
            .collect()
    }

    fn current_filter(&self) -> Option<&ActiveFilter> {
        self.visible_filters()
            .get(self.f_sel)
            .map(|&fi| &self.filters[fi])
    }

    fn key_filters(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('/') => {
                self.search.clear();
                self.searching = true;
                self.pane = Pane::Filters;
            }
            KeyCode::Char('s') => {
                self.gmode = match self.gmode {
                    GroupMode::Tool => GroupMode::Source,
                    GroupMode::Source => GroupMode::Tool,
                };
                self.g_sel = 0;
                self.sel_group = 0;
                self.f_sel = 0;
                self.search.clear();
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::Home | KeyCode::Char('g') => self.jump_sel(0),
            KeyCode::End | KeyCode::Char('G') => self.jump_sel(usize::MAX),
            _ => {}
        }
    }

    fn move_sel(&mut self, dir: i32) {
        match self.pane {
            Pane::Groups => {
                let len = self.groups().len();
                if len == 0 {
                    return;
                }
                self.g_sel = (self.g_sel as i32 + dir).rem_euclid(len as i32) as usize;
                self.sel_group = self.g_sel;
                self.f_sel = 0;
            }
            Pane::Filters => {
                let len = self.visible_filters().len();
                if len == 0 {
                    return;
                }
                self.f_sel = (self.f_sel as i32 + dir).rem_euclid(len as i32) as usize;
            }
        }
    }

    fn jump_sel(&mut self, target: usize) {
        match self.pane {
            Pane::Groups => {
                let len = self.groups().len();
                if len > 0 {
                    self.g_sel = target.min(len - 1);
                    self.sel_group = self.g_sel;
                    self.f_sel = 0;
                }
            }
            Pane::Filters => {
                let len = self.visible_filters().len();
                if len > 0 {
                    self.f_sel = target.min(len - 1);
                }
            }
        }
    }

    fn clamp(&mut self) {
        let flen = self.visible_filters().len();
        if self.f_sel >= flen {
            self.f_sel = flen.saturating_sub(1);
        }
    }

    // ── scroll / action tabs ─────────────────────────────────────────────

    fn key_scroll(&mut self, code: KeyCode) {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(15),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(15),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::Char('r') => {
                self.reports.remove(&self.cmd.index());
            }
            _ => {}
        }
    }

    // ── Stats dashboard (index + install actions) ────────────────────────

    /// The dashboard actions: 0 = index this repo, 1 = install hooks,
    /// 2 = install the binary on PATH.
    const STATS_ACTIONS: usize = 3;

    fn key_stats(&mut self, code: KeyCode) {
        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.stats_confirm = false;
                self.stats_msg = None;
                self.stats_sel = (self.stats_sel + 1) % Self::STATS_ACTIONS;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.stats_confirm = false;
                self.stats_msg = None;
                self.stats_sel = (self.stats_sel + Self::STATS_ACTIONS - 1) % Self::STATS_ACTIONS;
            }
            KeyCode::Enter => match self.stats_sel {
                // Index: non-destructive, runs in the foreground (see event_loop).
                0 => self.request_index = true,
                // Install actions write files, so a second Enter confirms.
                sel => {
                    if self.stats_confirm {
                        if sel == 1 {
                            self.run_install();
                        } else {
                            self.run_install_binary();
                        }
                        self.stats_confirm = false;
                    } else {
                        self.stats_confirm = true;
                    }
                }
            },
            _ => self.stats_confirm = false,
        }
    }

    fn run_install(&mut self) {
        let _ = capture(&["install-hook", "--tool", "all"]);
        self.agents = detect_agents();
        let installed = self.agents.iter().filter(|a| a.installed).count();
        self.stats_msg = Some(format!("Hooks installed · {installed}/4 agents wired."));
    }

    /// Self-execute `tokenix install-binary` and surface its (short) report.
    fn run_install_binary(&mut self) {
        let out = capture(&["install-binary"]);
        let msg = out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        self.stats_msg = Some(if msg.is_empty() {
            "install-binary finished".to_string()
        } else {
            msg
        });
    }

    // ── rendering ─────────────────────────────────────────────────────────

    fn draw(&self, f: &mut Frame) {
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());
        self.draw_tabs(f, rows[0]);
        match self.cmd {
            Cmd::Stats => self.draw_stats(f, rows[1]),
            Cmd::Filters => self.draw_filters(f, rows[1]),
            Cmd::Gain => self.draw_gain(f, rows[1]),
            Cmd::Secrets => self.draw_secrets(f, rows[1]),
            Cmd::Egress => self.draw_egress(f, rows[1]),
            _ => self.draw_report(f, rows[1]),
        }
        self.draw_footer(f, rows[2]);
    }

    fn draw_tabs(&self, f: &mut Frame, area: Rect) {
        let titles = Cmd::ALL.map(|c| c.title());
        let tabs = Tabs::new(titles)
            .select(self.cmd.index())
            .block(Block::bordered().title(" tokenix "))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD).cyan())
            .divider("│");
        f.render_widget(tabs, area);
    }

    fn draw_stats(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        for w in crate::WORDMARK {
            lines.push(Line::from(Span::styled(
                w,
                Style::default().cyan().add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(Span::styled(
            format!(" ($)  {}", crate::TAGLINE),
            Style::default().yellow(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "version   {}",
            env!("CARGO_PKG_VERSION")
        )));
        lines.push(Line::from(""));

        lines.push(Line::from("hooks".bold()));
        for a in &self.agents {
            let (mark, style) = if a.installed {
                ("✓", Style::default().green())
            } else {
                ("✗", Style::default().red())
            };
            let scope = if a.installed {
                a.scope.clone()
            } else {
                "not installed".to_string()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {mark} "), style),
                Span::raw(format!("{:<13}", a.name)),
                Span::styled(scope, Style::default().dim()),
            ]));
        }
        lines.push(Line::from(""));

        lines.push(Line::from("index (this repo)".bold()));
        match self.index_stats {
            Some((files, chunks, tokens)) => {
                lines.push(Line::from(format!("  files     {files}")));
                lines.push(Line::from(format!("  chunks    {chunks}")));
                lines.push(Line::from(format!(
                    "  tokens    {}",
                    crate::ui::format_num(tokens)
                )));
            }
            None => lines.push(Line::from("  not indexed".to_string().dim())),
        }
        lines.push(Line::from(""));

        // Actions: index + install, selectable with ↑↓ and run with Enter.
        lines.push(Line::from("actions".bold()));
        let index_note = match self.index_stats {
            Some((files, ..)) => format!("reindex this repo ({files} files)"),
            None => "build the index for this repo".to_string(),
        };
        let missing = self.agents.iter().filter(|a| !a.installed).count();
        let install_note = if missing == 0 {
            "all agents wired".to_string()
        } else {
            format!("wire {missing} missing agent(s) · writes config files")
        };
        let binary_note = match crate::global_bin_dir() {
            Some(dir) if crate::dir_on_path(&dir) => {
                format!("already global · {}", dir.display())
            }
            Some(dir) => format!("make `tokenix` runnable anywhere · {}", dir.display()),
            None => "cannot resolve the per-user bin directory".to_string(),
        };
        lines.push(action_line(
            self.stats_sel == 0,
            "Index repository",
            &index_note,
        ));
        lines.push(action_line(
            self.stats_sel == 1,
            "Install hooks (all)",
            &install_note,
        ));
        lines.push(action_line(
            self.stats_sel == 2,
            "Install binary (PATH)",
            &binary_note,
        ));

        if self.stats_confirm {
            let warn = if self.stats_sel == 2 {
                "⚠ copies the executable and updates your user PATH — press Enter to confirm"
            } else {
                "⚠ writes config files — press Enter to confirm"
            };
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                warn,
                Style::default().yellow().add_modifier(Modifier::BOLD),
            )));
        } else if let Some(msg) = &self.stats_msg {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("✓ {msg}"),
                Style::default().green(),
            )));
        }

        let p = Paragraph::new(Text::from(lines))
            .block(Block::bordered().title(" stats "))
            .scroll((self.scroll, 0));
        f.render_widget(p, area);
    }

    fn draw_filters(&self, f: &mut Frame, area: Rect) {
        let cols = Layout::horizontal([
            Constraint::Percentage(22),
            Constraint::Percentage(33),
            Constraint::Percentage(45),
        ])
        .split(area);
        self.draw_group_pane(f, cols[0]);
        self.draw_filter_pane(f, cols[1]);
        self.draw_preview_pane(f, cols[2]);
    }

    fn draw_group_pane(&self, f: &mut Frame, area: Rect) {
        let groups = self.groups();
        let items: Vec<ListItem> = groups
            .iter()
            .map(|g| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{:<14}", g.label)),
                    Span::styled(format!("{}", g.idxs.len()), Style::default().dim()),
                ]))
            })
            .collect();
        let kind = match self.gmode {
            GroupMode::Tool => "tool",
            GroupMode::Source => "source",
        };
        let title = format!(" by {kind}  {}/{} ", self.g_sel + 1, groups.len().max(1));
        let mut state = ListState::default();
        state.select(Some(self.g_sel.min(groups.len().saturating_sub(1))));
        f.render_stateful_widget(
            list_widget(items, title, self.pane == Pane::Groups),
            area,
            &mut state,
        );
    }

    fn draw_filter_pane(&self, f: &mut Frame, area: Rect) {
        let vis = self.visible_filters();
        let items: Vec<ListItem> = vis
            .iter()
            .map(|&fi| ListItem::new(self.filters[fi].name.clone()))
            .collect();
        let label = self
            .groups()
            .get(self.sel_group)
            .map(|g| g.label.clone())
            .unwrap_or_default();
        let title = if self.search.is_empty() {
            format!(" {label}  {}/{} ", self.f_sel + 1, vis.len().max(1))
        } else {
            format!(" {label}  /{}  {} hits ", self.search, vis.len())
        };
        let mut state = ListState::default();
        if !vis.is_empty() {
            state.select(Some(self.f_sel.min(vis.len() - 1)));
        }
        f.render_stateful_widget(
            list_widget(items, title, self.pane == Pane::Filters),
            area,
            &mut state,
        );
    }

    fn draw_preview_pane(&self, f: &mut Frame, area: Rect) {
        let rows = Layout::vertical([
            Constraint::Percentage(50),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

        let (name, sample, output) = match self.current_filter() {
            Some(af) => {
                let sample = self.samples.get(&af.name).cloned();
                let output = sample
                    .as_ref()
                    .map(|s| crate::filters::apply_filter(s, &af.filter));
                (af.name.clone(), sample, output)
            }
            None => (String::new(), None, None),
        };

        // Token counter: how many tokens the raw sample costs vs the filtered
        // output, so the X → Y saving is visible at a glance.
        let tokens_in = sample.as_deref().map(crate::chunker::count_tokens);
        let tokens_out = output.as_deref().map(crate::chunker::count_tokens);

        let input_title = match tokens_in {
            Some(t) => format!(" sample input · {name} · {t} tokens "),
            None => format!(" sample input · {name} "),
        };
        let input_text = sample.unwrap_or_else(|| "no embedded sample for this filter".to_string());
        let input = Paragraph::new(input_text)
            .block(Block::bordered().title(input_title))
            .wrap(Wrap { trim: false });
        f.render_widget(input, rows[0]);

        // X → Y gauge between the panes.
        let gauge = match (tokens_in, tokens_out) {
            (Some(x), Some(y)) => {
                let saved_frac = if x > 0 {
                    (x.saturating_sub(y)) as f64 / x as f64
                } else {
                    0.0
                };
                Line::from(vec![
                    Span::styled(format!(" {x} "), Style::default().yellow().bold()),
                    Span::styled("→", Style::default().dim()),
                    Span::styled(format!(" {y} tokens "), Style::default().cyan().bold()),
                    Span::styled(
                        format!("· {:.1}% saved ", saved_frac * 100.0),
                        Style::default().green().bold(),
                    ),
                    Span::styled(crate::ui::bar(saved_frac, 16), Style::default().green()),
                ])
            }
            _ => Line::from(Span::styled(
                " no sample — no token data ",
                Style::default().dim(),
            )),
        };
        f.render_widget(Paragraph::new(gauge), rows[1]);

        let output_title = match tokens_out {
            Some(t) => format!(" filtered output · {t} tokens "),
            None => " filtered output ".to_string(),
        };
        let output_text = output.unwrap_or_else(|| "—".to_string());
        let out = Paragraph::new(output_text)
            .block(
                Block::bordered()
                    .title(output_title)
                    .border_style(Style::default().cyan()),
            )
            .wrap(Wrap { trim: false });
        f.render_widget(out, rows[2]);
    }

    fn draw_gain(&self, f: &mut Frame, area: Rect) {
        let Some(view) = &self.gain_cache else {
            f.render_widget(
                Paragraph::new("computing…").block(Block::bordered().title(" gain ")),
                area,
            );
            return;
        };
        let s = &view.stats;
        let green = Style::default().green();
        let bold = Modifier::BOLD;
        let mut lines: Vec<Line> = Vec::new();

        let scope = if self.gain_global {
            "all projects"
        } else {
            "this repo"
        };
        lines.push(Line::from(vec![
            Span::styled("scope  ", Style::default().dim()),
            Span::styled(scope, Style::default().cyan().add_modifier(bold)),
            Span::styled(
                format!("   ·   {} hook calls", s.total_calls),
                Style::default().dim(),
            ),
        ]));
        lines.push(Line::from(""));

        if s.total_calls == 0 {
            lines.push(Line::from(
                "No hook events yet. Install hooks (Stats tab) and use your AI tool."
                    .to_string()
                    .yellow(),
            ));
            f.render_widget(
                Paragraph::new(Text::from(lines))
                    .block(Block::bordered().title(" gain "))
                    .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }

        // Headline: the one number this tab exists for.
        lines.push(Line::from(vec![
            Span::styled(" ✦ TOKENS SAVED  ", green.add_modifier(bold)),
            Span::styled(
                crate::ui::format_num(s.tokens_saved),
                green.add_modifier(bold),
            ),
            Span::styled(
                format!("  ·  {:.1}% less than without tokenix", s.pct_saved),
                Style::default().dim(),
            ),
        ]));
        let ref_model = crate::gain::MODELS.iter().find(|m| m.reference);
        if let Some(m) = ref_model {
            let usd = s.tokens_saved as f64 * m.input_per_1m / 1_000_000.0;
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(crate::ui::bar(s.pct_saved / 100.0, 28), green),
                Span::styled(
                    format!("  ≈ ${usd:.2} saved at {} input rates", m.name),
                    Style::default().dim(),
                ),
            ]));
        }
        lines.push(Line::from(""));

        lines.push(Line::from("token summary".bold()));
        lines.push(metric(
            "original",
            &crate::ui::format_num(s.tokens_original),
            Style::default().yellow(),
        ));
        lines.push(metric(
            "optimized",
            &crate::ui::format_num(s.tokens_used),
            Style::default().cyan(),
        ));
        lines.push(metric(
            "saved",
            &crate::ui::format_num(s.tokens_saved),
            green.add_modifier(bold),
        ));
        let ipct = s.intercepted as f64 / s.total_calls as f64 * 100.0;
        lines.push(Line::from(
            format!(
                "  {:<11}{} intercepted ({:.0}%) · {} passed",
                "calls", s.intercepted, ipct, s.passed
            )
            .dim(),
        ));
        lines.push(Line::from(""));

        // Where the savings came from: semantic index vs command filters.
        lines.push(Line::from("savings by source".bold()));
        let total_saved = s.tokens_saved.max(1);
        for (label, desc, saved, calls) in [
            (
                "semantic index",
                "Read outlines; Grep neutral context",
                s.index_saved,
                s.index_calls,
            ),
            (
                "command filters",
                "Bash/PowerShell output compression",
                s.filter_saved,
                s.filter_calls,
            ),
        ] {
            let share = saved as f64 / total_saved as f64;
            lines.push(Line::from(vec![
                Span::raw(format!("  {label:<17}")),
                Span::styled(
                    format!("{:>9}", crate::ui::format_num(saved)),
                    green.add_modifier(bold),
                ),
                Span::styled(format!("  {:>5.1}%  ", share * 100.0), green),
                Span::styled(crate::ui::bar(share, 14), Style::default().dim()),
                Span::styled(format!("  {calls} calls · {desc}"), Style::default().dim()),
            ]));
        }
        lines.push(Line::from(""));

        if !s.by_tool.is_empty() {
            lines.push(Line::from("by tool".bold()));
            let max = s
                .by_tool
                .iter()
                .map(|(_, _, v)| *v)
                .max()
                .unwrap_or(1)
                .max(1);
            for (tool, count, saved) in &s.by_tool {
                lines.push(bar_row(tool, *count, *saved, max));
            }
            lines.push(Line::from(""));
        }

        if !s.by_command.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("by command", Style::default().add_modifier(bold)),
                Span::styled("  · filter savings", Style::default().dim()),
            ]));
            lines.push(Line::from(
                format!(
                    "  {:>2}  {:<18} {:>5} {:>10}  {:>6}",
                    "#", "command", "calls", "saved", "share"
                )
                .dim(),
            ));
            let max = s
                .by_command
                .iter()
                .map(|(_, _, v)| *v)
                .max()
                .unwrap_or(1)
                .max(1);
            for (i, (cmd, count, saved)) in s.by_command.iter().take(12).enumerate() {
                let share = *saved as f64 / total_saved as f64;
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>2}  ", i + 1), Style::default().dim()),
                    Span::raw(format!("{:<18} ", trunc(cmd, 18))),
                    Span::styled(format!("{count:>5} "), Style::default().dim()),
                    Span::styled(format!("{:>10}  ", crate::ui::format_num(*saved)), green),
                    Span::styled(format!("{:>5.1}%  ", share * 100.0), Style::default().dim()),
                    Span::styled(
                        crate::ui::bar(*saved as f64 / max as f64, 12),
                        Style::default().dim(),
                    ),
                ]));
            }
            if s.by_command.len() > 12 {
                lines.push(Line::from(
                    format!("      … +{} more commands", s.by_command.len() - 12).dim(),
                ));
            }
            lines.push(Line::from(""));
        }

        if self.gain_global && !view.projects.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("by project", Style::default().add_modifier(bold)),
                Span::styled("  · tokens saved per repo", Style::default().dim()),
            ]));
            lines.push(Line::from(
                format!(
                    "  {:>2}  {:<22} {:>10}  {:>6}  {:<14} {}",
                    "#", "project", "saved", "share", "", "intercepted"
                )
                .dim(),
            ));
            let global_saved: i64 = view
                .projects
                .iter()
                .map(|(_, v, _, _)| *v)
                .sum::<i64>()
                .max(1);
            let max = view
                .projects
                .iter()
                .map(|(_, v, _, _)| *v)
                .max()
                .unwrap_or(1)
                .max(1);
            for (i, (label, saved, total, inter)) in view.projects.iter().take(15).enumerate() {
                let share = *saved as f64 / global_saved as f64;
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>2}  ", i + 1), Style::default().dim()),
                    Span::raw(format!("{:<22} ", trunc(&short_path(label), 22))),
                    Span::styled(format!("{:>10}  ", crate::ui::format_num(*saved)), green),
                    Span::styled(format!("{:>5.1}%  ", share * 100.0), Style::default().dim()),
                    Span::styled(
                        crate::ui::bar(*saved as f64 / max as f64, 12),
                        Style::default().dim(),
                    ),
                    Span::styled(format!("  {inter}/{total}"), Style::default().dim()),
                ]));
            }
            if view.projects.len() > 15 {
                lines.push(Line::from(
                    format!("      … +{} more projects", view.projects.len() - 15).dim(),
                ));
            }
            lines.push(Line::from(""));
        }

        if self.gain_cost {
            lines.push(Line::from(vec![
                Span::styled("cost estimate", Style::default().add_modifier(bold)),
                Span::styled("  · USD per 1M input tokens", Style::default().dim()),
            ]));
            lines.push(Line::from(
                format!(
                    "  {:<24}{:>8}{:>11}{:>11}{:>11}",
                    "model", "$/1M in", "without", "with", "saved"
                )
                .dim(),
            ));
            for row in &s.cost_rows {
                let price = crate::gain::MODELS
                    .iter()
                    .find(|m| m.name == row.model)
                    .map(|m| m.input_per_1m)
                    .unwrap_or(0.0);
                let marker = if row.reference { " ★" } else { "" };
                let name_style = if row.reference {
                    Style::default().cyan().add_modifier(bold)
                } else {
                    Style::default()
                };
                let saved_style = if row.reference {
                    green.add_modifier(bold)
                } else {
                    green
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<24}", format!("{}{}", row.model, marker)),
                        name_style,
                    ),
                    Span::raw(format!("{:>8}", format!("${price:.2}"))),
                    Span::styled(
                        format!("{:>11}", format!("${:.4}", row.without_usd)),
                        Style::default().yellow(),
                    ),
                    Span::styled(
                        format!("{:>11}", format!("${:.4}", row.with_usd)),
                        Style::default().cyan(),
                    ),
                    Span::styled(
                        format!("{:>11}", format!("${:.4}", row.saved_usd)),
                        saved_style,
                    ),
                ]));
            }
            lines.push(Line::from(
                format!(
                    "  ★ reference model · public provider prices collected {}",
                    crate::gain::PRICING_COLLECTED_AT
                )
                .dim(),
            ));
        } else {
            lines.push(Line::from(
                "press c for the per-model cost estimate".to_string().dim(),
            ));
        }

        f.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::bordered().title(" gain "))
                .scroll((self.scroll, 0)),
            area,
        );
    }

    fn draw_secrets(&self, f: &mut Frame, area: Rect) {
        // Still scanning.
        if self.secrets.is_none() {
            let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spin = frames[self.spinner % frames.len()];
            let mut lines = vec![Line::from("")];
            if let Some(msg) = &self.sec_msg {
                lines.push(Line::from(vec![
                    Span::styled("  ✓  ", Style::default().green()),
                    Span::styled(msg.clone(), Style::default().green()),
                ]));
                lines.push(Line::from(""));
            }
            lines.push(Line::from(vec![
                Span::styled(format!("  {spin}  "), Style::default().cyan()),
                Span::styled(
                    "Scanning AI agent conversations for credentials…",
                    Style::default().add_modifier(Modifier::BOLD).cyan(),
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(
                "  reading Claude · Copilot · Codex history (SQLite + JSON), redacting matches"
                    .to_string()
                    .dim(),
            ));
            f.render_widget(
                Paragraph::new(Text::from(lines)).block(Block::bordered().title(" secrets ")),
                area,
            );
            return;
        }

        let findings = self.secrets.as_ref().unwrap();
        let scanned: usize = self.secrets_counts.iter().map(|(_, n)| n).sum();

        // Clean — no findings.
        if findings.is_empty() {
            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        "  ✓  ",
                        Style::default().green().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "No credentials found in agent conversations.",
                        Style::default().green().add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(
                    format!(
                        "  scanned {scanned} files across {} agents",
                        self.secrets_counts.len()
                    )
                    .dim(),
                ),
            ];
            f.render_widget(
                Paragraph::new(Text::from(lines)).block(Block::bordered().title(" secrets ")),
                area,
            );
            return;
        }

        let cols = Layout::horizontal([
            Constraint::Percentage(26),
            Constraint::Percentage(30),
            Constraint::Percentage(44),
        ])
        .split(area);
        self.draw_sec_groups(f, cols[0], findings.len());
        self.draw_sec_items(f, cols[1]);
        self.draw_sec_detail(f, cols[2]);
    }

    fn draw_sec_groups(&self, f: &mut Frame, area: Rect, total: usize) {
        let items: Vec<ListItem> = self
            .sec_groups
            .iter()
            .map(|(label, idxs)| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<18}", trunc(label, 18)), Style::default().red()),
                    Span::styled(format!("{}", idxs.len()), Style::default().dim()),
                ]))
            })
            .collect();
        let kind = match self.sec_gmode {
            SecGroup::Rule => "rule",
            SecGroup::Agent => "agent",
        };
        let title = format!(" {total} secrets · by {kind} ");
        let mut state = ListState::default();
        state.select(Some(
            self.sec_g.min(self.sec_groups.len().saturating_sub(1)),
        ));
        f.render_stateful_widget(
            list_widget(items, title, self.sec_pane == Pane::Groups),
            area,
            &mut state,
        );
    }

    fn draw_sec_items(&self, f: &mut Frame, area: Rect) {
        let findings = match &self.secrets {
            Some(v) => v,
            None => return,
        };
        // One row per *distinct* secret value, with an occurrence count.
        let distinct = self.distinct_secrets();
        let items: Vec<ListItem> = distinct
            .iter()
            .map(|idxs| {
                let fnd = &findings[idxs[0]];
                let shown = if self.sec_reveal {
                    &fnd.secret
                } else {
                    &fnd.redacted
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<18}", trunc(shown, 18)),
                        Style::default().yellow(),
                    ),
                    Span::styled(format!("×{}", idxs.len()), Style::default().red()),
                ]))
            })
            .collect();
        let label = self
            .sec_groups
            .get(self.sec_sel_group)
            .map(|g| g.0.clone())
            .unwrap_or_default();
        let title = format!(
            " {}  {}/{} ",
            trunc(&label, 16),
            self.sec_i + 1,
            distinct.len().max(1)
        );
        let mut state = ListState::default();
        if !distinct.is_empty() {
            state.select(Some(self.sec_i.min(distinct.len() - 1)));
        }
        f.render_stateful_widget(
            list_widget(items, title, self.sec_pane == Pane::Filters),
            area,
            &mut state,
        );
    }

    fn draw_sec_detail(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        let occ = self.current_occurrences();
        if let (Some(findings), Some(&rep)) = (&self.secrets, occ.first()) {
            let fnd = &findings[rep];
            lines.push(Line::from(Span::styled(
                fnd.rule.clone(),
                Style::default().red().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            let shown = if self.sec_reveal {
                fnd.secret.clone()
            } else {
                fnd.redacted.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{:<10}", "value"), Style::default().dim()),
                Span::styled(shown, Style::default().yellow()),
            ]));
            lines.push(Line::from(vec![
                Span::styled(format!("{:<10}", ""), Style::default().dim()),
                Span::styled(
                    format!(
                        "{} chars · {} occurrence(s){}",
                        fnd.length,
                        occ.len(),
                        if self.sec_reveal {
                            ""
                        } else {
                            " · v to reveal"
                        }
                    ),
                    Style::default().dim(),
                ),
            ]));
            if let Some(repo) = &fnd.repo {
                lines.push(kv("repo", repo));
            }
            if let Some(branch) = &fnd.branch {
                lines.push(kv("branch", branch));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("occurrences".bold()));
            for &i in occ.iter().take(20) {
                let o = &findings[i];
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<11}", trunc(&o.agent, 11)),
                        Style::default().cyan(),
                    ),
                    Span::styled(
                        format!("{}:{}", trunc(&o.file, 40), o.line),
                        Style::default().dim(),
                    ),
                ]));
            }
            if occ.len() > 20 {
                lines.push(Line::from(format!("  … +{} more", occ.len() - 20).dim()));
            }
            lines.push(Line::from(""));
            if self.sec_confirm {
                let editable = occ.iter().filter(|&&i| !is_db(&findings[i].path)).count();
                lines.push(Line::from(Span::styled(
                    format!("⚠ write [REDACTED] over this value in {editable} file(s)?"),
                    Style::default().yellow().add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(
                    "  press x again to confirm · any other key cancels"
                        .to_string()
                        .dim(),
                ));
            } else {
                lines.push(Line::from(
                    "v: reveal · c: copy · x: replace with [REDACTED] · rotate the credential"
                        .to_string()
                        .dim(),
                ));
            }
            if let Some(msg) = &self.sec_msg {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("✓ {msg}"),
                    Style::default().green(),
                )));
            }
        }
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::bordered().title(" finding "))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_egress(&self, f: &mut Frame, area: Rect) {
        // Still scanning.
        if self.egress.is_none() {
            let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let spin = frames[self.spinner % frames.len()];
            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!("  {spin}  "), Style::default().cyan()),
                    Span::styled(
                        "Scanning AI agent conversations for external destinations…",
                        Style::default().add_modifier(Modifier::BOLD).cyan(),
                    ),
                ]),
                Line::from(""),
                Line::from(
                    "  reading Claude · Copilot · Codex history, collecting DNS/IPs"
                        .to_string()
                        .dim(),
                ),
            ];
            f.render_widget(
                Paragraph::new(Text::from(lines)).block(Block::bordered().title(" egress ")),
                area,
            );
            return;
        }

        let findings = self.egress.as_ref().unwrap();
        let scanned: usize = self.egress_counts.iter().map(|(_, n)| n).sum();

        if findings.is_empty() {
            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        "  ✓  ",
                        Style::default().green().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "No external destinations found in agent conversations.",
                        Style::default().green().add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(
                    format!(
                        "  scanned {scanned} files across {} agents",
                        self.egress_counts.len()
                    )
                    .dim(),
                ),
            ];
            f.render_widget(
                Paragraph::new(Text::from(lines)).block(Block::bordered().title(" egress ")),
                area,
            );
            return;
        }

        let cols = Layout::horizontal([
            Constraint::Percentage(26),
            Constraint::Percentage(30),
            Constraint::Percentage(44),
        ])
        .split(area);
        self.draw_egr_groups(f, cols[0], findings.len(), scanned);
        self.draw_egr_items(f, cols[1]);
        self.draw_egr_detail(f, cols[2]);
    }

    fn draw_egr_groups(&self, f: &mut Frame, area: Rect, total: usize, scanned: usize) {
        let items: Vec<ListItem> = self
            .egr_groups
            .iter()
            .map(|(label, idxs)| {
                let label_style = if self.egr_gmode == EgrGroup::Host {
                    self.egress_host_style(label)
                } else {
                    Style::default().cyan()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<18}", trunc(label, 18)), label_style),
                    Span::styled(format!("{}", idxs.len()), Style::default().dim()),
                ]))
            })
            .collect();
        let group_label = match self.egr_gmode {
            EgrGroup::Host => "host",
            EgrGroup::Rule => "rule",
            EgrGroup::Agent => "agent",
            EgrGroup::File => "file",
        };
        let title = format!(" {total} egress · by {group_label} · {scanned} files ");
        let mut state = ListState::default();
        state.select(Some(
            self.egr_g.min(self.egr_groups.len().saturating_sub(1)),
        ));
        f.render_stateful_widget(
            list_widget(items, title, self.egr_pane == Pane::Groups),
            area,
            &mut state,
        );
    }

    fn draw_egr_items(&self, f: &mut Frame, area: Rect) {
        let findings = match &self.egress {
            Some(v) => v,
            None => return,
        };
        let distinct = self.distinct_egress_targets();
        let items: Vec<ListItem> = distinct
            .iter()
            .map(|idxs| {
                let fnd = &findings[idxs[0]];
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<16}", trunc(&fnd.host, 16)),
                        self.egress_host_style(&fnd.host),
                    ),
                    Span::styled(format!("  {}", trunc(&fnd.target, 28)), Style::default()),
                    Span::styled(format!(" ×{}", idxs.len()), Style::default().cyan()),
                ]))
            })
            .collect();
        let label = self
            .egr_groups
            .get(self.egr_sel_group)
            .map(|g| g.0.clone())
            .unwrap_or_default();
        let title = format!(
            " {}  {}/{} ",
            trunc(&label, 16),
            self.egr_i + 1,
            distinct.len().max(1)
        );
        let mut state = ListState::default();
        if !distinct.is_empty() {
            state.select(Some(self.egr_i.min(distinct.len() - 1)));
        }
        f.render_stateful_widget(
            list_widget(items, title, self.egr_pane == Pane::Filters),
            area,
            &mut state,
        );
    }

    fn draw_egr_detail(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        let occ = self.current_egress_occurrences();
        if let (Some(findings), Some(&rep)) = (&self.egress, occ.first()) {
            let fnd = &findings[rep];
            lines.push(Line::from(Span::styled(
                fnd.host.clone(),
                self.egress_host_style(&fnd.host)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(kv("rule", &fnd.rule));
            lines.push(kv("target", &fnd.target));
            lines.push(kv("agent", &fnd.agent));
            if let Some(repo) = &fnd.repo {
                lines.push(kv("repo", repo));
            }
            if let Some(branch) = &fnd.branch {
                lines.push(kv("branch", branch));
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{:<10}", "hits"), Style::default().dim()),
                Span::styled(format!("{}", occ.len()), Style::default().cyan()),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from("occurrences".bold()));
            for &i in occ.iter().take(20) {
                let o = &findings[i];
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<11}", trunc(&o.agent, 11)),
                        Style::default().cyan(),
                    ),
                    Span::styled(
                        format!("{}:{}", trunc(&o.file, 40), o.line),
                        Style::default().dim(),
                    ),
                ]));
            }
            if occ.len() > 20 {
                lines.push(Line::from(format!("  … +{} more", occ.len() - 20).dim()));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(
                "s: group by host/rule/agent/file · investigate unexpected outbound traffic"
                    .to_string()
                    .dim(),
            ));
        }
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::bordered().title(" destination "))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn egress_host_style(&self, host: &str) -> Style {
        match self.egr_reputation.verdict(host) {
            crate::egress_scan::HostVerdict::Safe => Style::default().green(),
            crate::egress_scan::HostVerdict::Dangerous => Style::default().red(),
            crate::egress_scan::HostVerdict::Unknown => Style::default().yellow(),
        }
    }

    fn draw_report(&self, f: &mut Frame, area: Rect) {
        let body = self
            .reports
            .get(&self.cmd.index())
            .cloned()
            .unwrap_or_else(|| "loading…".to_string());
        let p = Paragraph::new(body)
            .block(Block::bordered().title(format!(" {} ", self.cmd.title().to_lowercase())))
            .scroll((self.scroll, 0));
        f.render_widget(p, area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let hint = if self.searching {
            format!("/{}_   Enter: apply · Esc: clear", self.search)
        } else {
            match self.cmd {
                Cmd::Filters => {
                    "←→: tab · Tab: pane · ↑↓: move · /: search · s: tool/source · q: quit"
                        .to_string()
                }
                Cmd::Gain => {
                    "←→: tab · ↑↓: scroll · c: cost · a: all-projects · r: refresh · q: quit"
                        .to_string()
                }
                Cmd::Secrets if self.secrets.is_none() => {
                    "scanning… · ←→: tab · q: quit".to_string()
                }
                Cmd::Secrets if self.sec_confirm => {
                    "x: confirm [REDACTED] write · any other key cancels".to_string()
                }
                Cmd::Secrets => {
                    "←→: tab · Tab: pane · ↑↓: move · s: group · v: reveal · c: copy · x: redact · r: rescan · q"
                        .to_string()
                }
                Cmd::Egress if self.egress.is_none() => {
                    "scanning… · ←→: tab · q: quit".to_string()
                }
                Cmd::Egress => {
                    "←→: tab · Tab: pane · ↑↓: move · s: group · r: rescan · q: quit"
                        .to_string()
                }
                Cmd::Stats if self.stats_confirm => {
                    "Enter: confirm install (writes files) · any other key cancels".to_string()
                }
                Cmd::Stats => "←→: tab · ↑↓: action · Enter: run · q: quit".to_string(),
                _ => "←→: tab · ↑↓/PgUp/PgDn: scroll · r: refresh · q: quit".to_string(),
            }
        };
        f.render_widget(Paragraph::new(hint.dim()), area);
    }
}

/// A selectable dashboard action row: `▸ Label   note`.
fn action_line(selected: bool, label: &str, note: &str) -> Line<'static> {
    let (cursor, label_style) = if selected {
        ("▸ ", Style::default().cyan().add_modifier(Modifier::BOLD))
    } else {
        ("  ", Style::default())
    };
    Line::from(vec![
        Span::styled(format!("{cursor}{label:<20}"), label_style),
        Span::styled(note.to_string(), Style::default().dim()),
    ])
}

/// A focus-aware bordered list with reverse-video selection.
fn list_widget(items: Vec<ListItem>, title: String, focused: bool) -> List {
    let items = if items.is_empty() {
        vec![ListItem::new(Span::styled(
            "— no results —",
            Style::default().dim(),
        ))]
    } else {
        items
    };
    let mut block = Block::bordered().title(title);
    if focused {
        block = block.border_style(Style::default().cyan());
    }
    List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ")
}

fn other_pane(p: Pane) -> Pane {
    match p {
        Pane::Groups => Pane::Filters,
        Pane::Filters => Pane::Groups,
    }
}

/// Copy `text` to the system clipboard via the platform's native utility
/// (clip.exe / pbcopy / wl-copy / xclip / xsel) — no extra dependencies.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--input", "--clipboard"]),
        ]
    };
    let mut last_err = "no clipboard utility found".to_string();
    for (cmd, args) in candidates {
        match Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(stdin) = child.stdin.as_mut() {
                    if let Err(e) = stdin.write_all(text.as_bytes()) {
                        last_err = e.to_string();
                        continue;
                    }
                }
                match child.wait() {
                    Ok(s) if s.success() => return Ok(()),
                    Ok(s) => last_err = format!("{cmd} exited with {s}"),
                    Err(e) => last_err = e.to_string(),
                }
            }
            Err(e) => last_err = format!("{cmd}: {e}"),
        }
    }
    Err(last_err)
}

/// SQLite databases can't be string-redacted in place (the byte edit corrupts
/// them), so the redact action skips them.
fn is_db(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "db" | "sqlite" | "sqlite3" | "vscdb"
    )
}

/// `  key       value` — a dim-labelled detail row.
fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<10}"), Style::default().dim()),
        Span::raw(value.to_string()),
    ])
}

/// `  label      value` with the value styled — a gain summary metric row.
fn metric(label: &str, value: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("  {label:<11}")),
        Span::styled(value.to_string(), style),
    ])
}

/// `  name   count   saved  ████░░` — a labelled bar row for by-tool / by-command.
fn bar_row(label: &str, count: usize, saved: i64, max: i64) -> Line<'static> {
    let frac = saved as f64 / max.max(1) as f64;
    Line::from(vec![
        Span::raw(format!("  {:<16}", trunc(label, 16))),
        Span::styled(format!("{count:>5} "), Style::default().dim()),
        Span::styled(
            format!("{:>9}  ", crate::ui::format_num(saved)),
            Style::default().green(),
        ),
        Span::styled(crate::ui::bar(frac, 14), Style::default().dim()),
    ])
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// Last two path components, to keep project labels compact.
fn short_path(p: &str) -> String {
    let path = std::path::Path::new(p);
    let parts: Vec<_> = path.components().collect();
    if parts.len() >= 2 {
        parts[parts.len() - 2..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    } else {
        p.to_string()
    }
}

/// Run the tokenix binary again with `args` and return its stdout (stderr if
/// stdout is empty). Output is plain text — colors auto-disable off a TTY.
fn capture(args: &[&str]) -> String {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return format!("cannot locate tokenix binary: {e}"),
    };
    match std::process::Command::new(exe).args(args).output() {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout);
            if out.trim().is_empty() {
                String::from_utf8_lossy(&o.stderr).into_owned()
            } else {
                out.into_owned()
            }
        }
        Err(e) => format!("failed to run `tokenix {}`: {e}", args.join(" ")),
    }
}

/// Index the current repo with inherited stdio (so the progress bar shows), then
/// wait for a keypress before the caller restores the TUI.
fn run_index_foreground() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    println!("\nIndexing this repository… (Ctrl-C to abort)\n");
    let _ = std::process::Command::new(exe).arg("index").status();
    print!("\nPress Enter to return to tokenix…");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn read_index_stats() -> Option<(i64, i64, i64)> {
    let cwd = std::env::current_dir().ok()?;
    let root = crate::store::find_project_root(&cwd);
    let conn = crate::store::open_db(&root, false).ok()??;
    let s = crate::store::count_stats(&conn).ok()?;
    Some((s.files, s.chunks, s.total_tokens))
}

/// Tool prefix of a filter name: the segment before the first `-`
/// (`cargo-test` → `cargo`, `git-diff` → `git`, `bat` → `bat`).
fn tool_of(name: &str) -> &str {
    name.split_once('-').map(|(p, _)| p).unwrap_or(name)
}

fn group_by(filters: &[ActiveFilter], key: impl Fn(&ActiveFilter) -> String) -> Vec<Group> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, af) in filters.iter().enumerate() {
        map.entry(key(af)).or_default().push(i);
    }
    map.into_iter()
        .map(|(label, mut idxs)| {
            idxs.sort_by(|&a, &b| filters[a].name.cmp(&filters[b].name));
            Group { label, idxs }
        })
        .collect()
}

/// Detect which AI harnesses have the tokenix hook/MCP wired, by checking the
/// known config files for a `tokenix` marker. Read-only.
fn detect_agents() -> Vec<AgentStatus> {
    let home = dirs::home_dir().unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let repo = crate::store::find_project_root(&cwd);

    vec![
        status(
            "Claude Code",
            &[
                (home.join(".claude/settings.json"), "global"),
                (repo.join(".claude/settings.local.json"), "local"),
            ],
        ),
        status(
            "Copilot",
            &[
                (home.join(".copilot/hooks/tokenix.json"), "global"),
                (repo.join(".github/hooks/hooks.json"), "local"),
            ],
        ),
        status("Codex", &[(home.join(".codex/hooks.json"), "global")]),
        status("OpenCode", &[(repo.join("opencode.json"), "local")]),
        status(
            "Antigravity",
            &[
                (
                    home.join(".gemini/config/plugins/tokenix/plugin.json"),
                    "global",
                ),
                (repo.join(".agents/plugins/tokenix/plugin.json"), "local"),
            ],
        ),
    ]
}

fn status(name: &'static str, candidates: &[(std::path::PathBuf, &str)]) -> AgentStatus {
    let mut installed = false;
    let mut scopes: Vec<&str> = Vec::new();
    for (path, scope) in candidates {
        if file_has_tokenix(path) {
            installed = true;
            scopes.push(scope);
        }
    }
    AgentStatus {
        name,
        installed,
        scope: scopes.join(" + "),
    }
}

fn file_has_tokenix(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.contains("tokenix"))
        .unwrap_or(false)
}
