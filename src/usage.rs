//! Absolute token-spend analytics from local agent transcripts.
//!
//! This is the spend-side counterpart to `gain` (which measures savings): it
//! reads the real `usage` blocks AI agents write to their on-disk histories and
//! reports how many tokens were consumed and the estimated USD cost, broken down
//! by day/week/month/session/model/project, plus rolling 5-hour billing blocks.

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};
use colored::Colorize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::gain::{price_for, usage_cost};

/// Length of a billing block (Anthropic's rolling 5-hour window).
const BLOCK_HOURS: i64 = 5;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[clap(rename_all = "kebab-case")]
pub enum Group {
    Daily,
    Weekly,
    Monthly,
    Session,
    Model,
    Project,
    Blocks,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[clap(rename_all = "kebab-case")]
pub enum CostMode {
    /// Use the cost logged by the agent when present, otherwise calculate it.
    Auto,
    /// Always calculate from token counts and the bundled pricing table.
    Calculate,
    /// Only show costs the agent itself logged.
    Display,
}

pub struct Options {
    pub group: Group,
    pub since: Option<String>,
    pub until: Option<String>,
    pub all_projects: bool,
    pub cost_mode: CostMode,
    pub statusline: bool,
    pub json: bool,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
struct Record {
    ts: DateTime<Local>,
    model: String,
    project: String,
    session: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    logged_cost: Option<f64>,
}

impl Record {
    fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn cost(&self, mode: CostMode) -> f64 {
        let calc = price_for(&self.model)
            .map(|p| {
                usage_cost(
                    p,
                    self.input,
                    self.output,
                    self.cache_read,
                    self.cache_write,
                )
            })
            .unwrap_or(0.0);
        match mode {
            CostMode::Calculate => calc,
            CostMode::Display => self.logged_cost.unwrap_or(0.0),
            CostMode::Auto => self.logged_cost.unwrap_or(calc),
        }
    }
}

#[derive(Default, Serialize)]
struct Row {
    key: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    tokens: u64,
    cost_usd: f64,
}

pub fn run(opts: Options) -> Result<()> {
    let records = collect_records(&opts)?;

    if opts.statusline {
        print_statusline(&records, opts.cost_mode);
        return Ok(());
    }

    if matches!(opts.group, Group::Blocks) {
        return report_blocks(&records, &opts);
    }

    let mut rows = aggregate(&records, &opts);
    sort_rows(&mut rows, opts.group);

    let total = totals(&records, opts.cost_mode);
    if opts.json {
        let forecast = month_forecast(&records, opts.cost_mode);
        let out = serde_json::json!({
            "group": format!("{:?}", opts.group).to_lowercase(),
            "rows": rows,
            "total": total,
            "month_forecast_usd": forecast,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print_table(&rows, &total, &opts);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

fn collect_records(opts: &Options) -> Result<Vec<Record>> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let since = opts.since.as_deref().and_then(parse_date);
    let until = opts.until.as_deref().and_then(parse_date);
    let scope = if opts.all_projects {
        None
    } else {
        Some(current_project(&opts.path))
    };

    let mut records = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (agent_key, root) in crate::transcripts::roots(&home) {
        if !root.exists() {
            continue;
        }
        for path in crate::transcripts::transcript_files(&root, agent_key) {
            let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
            if !matches!(ext, "jsonl" | "json") {
                continue;
            }
            parse_file(&path, &mut records, &mut seen);
        }
    }

    records.retain(|r| {
        let d = r.ts.date_naive();
        since.map(|s| d >= s).unwrap_or(true)
            && until.map(|u| d <= u).unwrap_or(true)
            && scope.as_deref().map(|s| r.project == s).unwrap_or(true)
    });
    records.sort_by_key(|r| r.ts);
    Ok(records)
}

fn parse_file(path: &Path, out: &mut Vec<Record>, seen: &mut HashSet<String>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let session_fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(rec) = record_from_value(&v, &session_fallback, seen) {
            out.push(rec);
        }
    }
}

fn record_from_value(
    v: &Value,
    session_fallback: &str,
    seen: &mut HashSet<String>,
) -> Option<Record> {
    let message = v.get("message");
    let usage = message
        .and_then(|m| m.get("usage"))
        .or_else(|| v.get("usage"))?;

    let input = u64_at(usage, "input_tokens");
    let output = u64_at(usage, "output_tokens");
    let cache_read = u64_at(usage, "cache_read_input_tokens");
    let cache_write = u64_at(usage, "cache_creation_input_tokens");
    if input + output + cache_read + cache_write == 0 {
        return None;
    }

    // Dedup replayed lines by (message id, requestId) when both are present.
    let msg_id = message
        .and_then(|m| m.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let req_id = v.get("requestId").and_then(|x| x.as_str()).unwrap_or("");
    if !msg_id.is_empty() && !req_id.is_empty() {
        let key = format!("{msg_id}|{req_id}");
        if !seen.insert(key) {
            return None;
        }
    }

    let ts = v
        .get("timestamp")
        .and_then(|x| x.as_str())
        .and_then(parse_ts)
        .unwrap_or_else(Local::now);

    let model = message
        .and_then(|m| m.get("model"))
        .or_else(|| v.get("model"))
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();

    let project = v
        .get("cwd")
        .and_then(|x| x.as_str())
        .map(basename)
        .unwrap_or_else(|| "?".to_string());

    let session = v
        .get("sessionId")
        .and_then(|x| x.as_str())
        .unwrap_or(session_fallback)
        .to_string();

    let logged_cost = v
        .get("costUSD")
        .or_else(|| v.get("cost_usd"))
        .and_then(|x| x.as_f64());

    Some(Record {
        ts,
        model,
        project,
        session,
        input,
        output,
        cache_read,
        cache_write,
        logged_cost,
    })
}

fn u64_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn parse_ts(s: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Local))
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

fn basename(p: &str) -> String {
    p.replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(p)
        .to_string()
}

fn current_project(path: &Path) -> String {
    std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| basename(&path.to_string_lossy()))
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

fn group_key(r: &Record, group: Group) -> String {
    match group {
        Group::Daily => r.ts.format("%Y-%m-%d").to_string(),
        Group::Weekly => {
            let iso = r.ts.iso_week();
            format!("{}-W{:02}", iso.year(), iso.week())
        }
        Group::Monthly => r.ts.format("%Y-%m").to_string(),
        Group::Session => short(&r.session),
        Group::Model => r.model.clone(),
        Group::Project => r.project.clone(),
        Group::Blocks => unreachable!(),
    }
}

fn aggregate(records: &[Record], opts: &Options) -> Vec<Row> {
    use std::collections::HashMap;
    let mut map: HashMap<String, Row> = HashMap::new();
    for r in records {
        let key = group_key(r, opts.group);
        let row = map.entry(key.clone()).or_insert_with(|| Row {
            key,
            ..Default::default()
        });
        row.input += r.input;
        row.output += r.output;
        row.cache_read += r.cache_read;
        row.cache_write += r.cache_write;
        row.tokens += r.tokens();
        row.cost_usd += r.cost(opts.cost_mode);
    }
    map.into_values().collect()
}

fn sort_rows(rows: &mut [Row], group: Group) {
    match group {
        // Chronological keys read best ascending.
        Group::Daily | Group::Weekly | Group::Monthly => rows.sort_by(|a, b| a.key.cmp(&b.key)),
        // Everything else: biggest spenders first.
        _ => rows.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd)),
    }
}

fn totals(records: &[Record], mode: CostMode) -> Row {
    let mut t = Row {
        key: "TOTAL".to_string(),
        ..Default::default()
    };
    for r in records {
        t.input += r.input;
        t.output += r.output;
        t.cache_read += r.cache_read;
        t.cache_write += r.cache_write;
        t.tokens += r.tokens();
        t.cost_usd += r.cost(mode);
    }
    t
}

/// Linear month-end projection from spend so far this calendar month.
fn month_forecast(records: &[Record], mode: CostMode) -> f64 {
    let now = Local::now();
    let month_cost: f64 = records
        .iter()
        .filter(|r| r.ts.year() == now.year() && r.ts.month() == now.month())
        .map(|r| r.cost(mode))
        .sum();
    let days_in_month = days_in_month(now.year(), now.month());
    let day = now.day().max(1);
    if day == 0 {
        return month_cost;
    }
    month_cost / day as f64 * days_in_month as f64
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|first_next| first_next.pred_opt())
        .map(|d| d.day())
        .unwrap_or(30)
}

// ---------------------------------------------------------------------------
// Blocks (5-hour billing windows)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Block {
    start: String,
    end: String,
    tokens: u64,
    cost_usd: f64,
    active: bool,
    burn_per_min: Option<f64>,
    projected_cost_usd: Option<f64>,
}

fn report_blocks(records: &[Record], opts: &Options) -> Result<()> {
    let mut blocks: Vec<Block> = Vec::new();
    let now = Local::now();
    let mut iter = records.iter();
    if let Some(first) = iter.next() {
        let mut start = floor_hour(first.ts);
        let mut tok = 0u64;
        let mut cost = 0.0;
        let mut latest = first.ts;
        let flush = |start: DateTime<Local>,
                     latest: DateTime<Local>,
                     tok: u64,
                     cost: f64,
                     now: DateTime<Local>|
         -> Block {
            let end = start + Duration::hours(BLOCK_HOURS);
            let active = now < end && now >= start;
            let (burn, proj) = if active {
                let mins = (now - start).num_minutes().max(1) as f64;
                let burn = tok as f64 / mins;
                let remaining = (end - now).num_minutes().max(0) as f64;
                let proj = cost + (cost / mins) * remaining;
                (Some(burn), Some(proj))
            } else {
                (None, None)
            };
            let _ = latest;
            Block {
                start: start.format("%Y-%m-%d %H:%M").to_string(),
                end: end.format("%H:%M").to_string(),
                tokens: tok,
                cost_usd: cost,
                active,
                burn_per_min: burn,
                projected_cost_usd: proj,
            }
        };
        for r in std::iter::once(first).chain(iter) {
            if r.ts >= start + Duration::hours(BLOCK_HOURS) {
                blocks.push(flush(start, latest, tok, cost, now));
                start = floor_hour(r.ts);
                tok = 0;
                cost = 0.0;
            }
            tok += r.tokens();
            cost += r.cost(opts.cost_mode);
            latest = r.ts;
        }
        blocks.push(flush(start, latest, tok, cost, now));
    }

    if opts.json {
        println!("{}", serde_json::to_string_pretty(&blocks)?);
        return Ok(());
    }

    println!("{}", "Usage — 5-hour blocks".bold());
    if blocks.is_empty() {
        println!("  {}", "no usage records found".dimmed());
        return Ok(());
    }
    for b in &blocks {
        let marker = if b.active {
            " ● active".green().to_string()
        } else {
            String::new()
        };
        println!(
            "  {}→{}  {:>10}  {:>9}{}",
            b.start,
            b.end,
            fmt_tokens(b.tokens),
            fmt_cost(b.cost_usd),
            marker
        );
        if b.active {
            if let (Some(burn), Some(proj)) = (b.burn_per_min, b.projected_cost_usd) {
                println!(
                    "          🔥 {:.0} tok/min · projected {}",
                    burn,
                    fmt_cost(proj)
                );
            }
        }
    }
    Ok(())
}

fn floor_hour(ts: DateTime<Local>) -> DateTime<Local> {
    Local
        .with_ymd_and_hms(ts.year(), ts.month(), ts.day(), ts.hour(), 0, 0)
        .single()
        .unwrap_or(ts)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn print_table(rows: &[Row], total: &Row, opts: &Options) {
    let title = format!("Usage — {:?}", opts.group).to_lowercase();
    println!("{}", title.bold());
    if rows.is_empty() {
        println!("  {}", "no usage records found".dimmed());
        return;
    }
    println!(
        "  {:<22} {:>10} {:>10} {:>10} {:>10} {:>11}",
        "key".dimmed(),
        "input".dimmed(),
        "output".dimmed(),
        "cache".dimmed(),
        "tokens".dimmed(),
        "cost".dimmed()
    );
    for r in rows {
        println!(
            "  {:<22} {:>10} {:>10} {:>10} {:>10} {:>11}",
            short(&r.key),
            fmt_tokens(r.input),
            fmt_tokens(r.output),
            fmt_tokens(r.cache_read + r.cache_write),
            fmt_tokens(r.tokens),
            fmt_cost(r.cost_usd)
        );
    }
    println!(
        "  {:<22} {:>10} {:>10} {:>10} {:>10} {:>11}",
        "TOTAL".bold(),
        fmt_tokens(total.input),
        fmt_tokens(total.output),
        fmt_tokens(total.cache_read + total.cache_write),
        fmt_tokens(total.tokens),
        fmt_cost(total.cost_usd).bold()
    );
    if matches!(opts.group, Group::Daily | Group::Monthly | Group::Weekly) {
        // Forecast needs the unfiltered-by-group records; recompute cheaply here.
        let forecast = total_month_forecast(opts);
        if let Some(f) = forecast {
            println!(
                "  {} {}",
                "month-end forecast:".dimmed(),
                fmt_cost(f).yellow()
            );
        }
    }
}

fn total_month_forecast(opts: &Options) -> Option<f64> {
    // Recollect is cheap relative to I/O already done; reuse collection.
    let records = collect_records(opts).ok()?;
    Some(month_forecast(&records, opts.cost_mode))
}

fn print_statusline(records: &[Record], mode: CostMode) {
    let now = Local::now();
    let today = now.date_naive();
    let mut cost = 0.0;
    let mut tokens = 0u64;
    for r in records.iter().filter(|r| r.ts.date_naive() == today) {
        cost += r.cost(mode);
        tokens += r.tokens();
    }
    // Active-block burn rate.
    let block_start = records
        .iter()
        .rev()
        .find(|r| now - r.ts < Duration::hours(BLOCK_HOURS))
        .map(|_| floor_hour(now - Duration::hours(BLOCK_HOURS)));
    let burn = block_start.map(|_| {
        let win_start = now - Duration::hours(BLOCK_HOURS);
        let tok: u64 = records
            .iter()
            .filter(|r| r.ts >= win_start)
            .map(|r| r.tokens())
            .sum();
        tok as f64 / (BLOCK_HOURS * 60) as f64
    });
    let mut parts = vec![
        format!("{} today", fmt_cost(cost)),
        format!("{} tok", fmt_tokens(tokens)),
    ];
    if let Some(b) = burn {
        parts.push(format!("🔥{:.0}/min", b));
    }
    println!("{}", parts.join(" · "));
}

fn short(s: &str) -> String {
    if s.len() > 20 {
        format!("{}…", &s[..19])
    } else {
        s.to_string()
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_cost(c: f64) -> String {
    if c >= 100.0 {
        format!("${:.0}", c)
    } else if c >= 1.0 {
        format!("${:.2}", c)
    } else {
        format!("${:.4}", c)
    }
}
