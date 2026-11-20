//! Absolute token-spend analytics from local agent transcripts.
//!
//! This is the spend-side counterpart to `gain` (which measures savings): it
//! reads the real `usage` blocks AI agents write to their on-disk histories and
//! reports how many tokens were consumed and the estimated USD cost, broken down
//! by day/week/month/session/model/project, plus rolling 5-hour billing blocks.

use anyhow::Result;
use colored::Colorize;
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp, ZonedDateTime};
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
    ts: Timestamp,
    model: String,
    project: String,
    session: String,
    input: u64,
    output: u64,
    cache_read: u64,
    /// Every cache-write token, whatever its lifetime.
    cache_write: u64,
    /// The part of `cache_write` written to the 1-hour cache (billed at 2x
    /// input instead of 1.25x). Zero when the record carries no split.
    cache_write_1h: u64,
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
                    self.cache_write_1h,
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

/// Aggregate spend mix across all projects' transcripts — the raw material
/// for `gain --economics` (pricing measured savings against the user's OWN
/// cost-per-token instead of list-price hypotheticals).
pub struct SpendMix {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
}

pub fn spend_mix() -> Option<SpendMix> {
    let opts = Options {
        group: Group::Model,
        since: None,
        until: None,
        all_projects: true,
        cost_mode: CostMode::Auto,
        statusline: false,
        json: false,
        path: PathBuf::from("."),
    };
    let records = collect_records(&opts).ok()?;
    if records.is_empty() {
        return None;
    }
    let mut mix = SpendMix {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cost_usd: 0.0,
    };
    for r in &records {
        mix.input += r.input;
        mix.output += r.output;
        mix.cache_read += r.cache_read;
        mix.cache_write += r.cache_write;
        mix.cost_usd += r.cost(CostMode::Auto);
    }
    Some(mix)
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
        print_table(&rows, &total, &opts, &records);
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
        let d = local(r.ts).date();
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
    // Claude Code splits the write total by cache lifetime:
    // `cache_creation: {ephemeral_5m_input_tokens, ephemeral_1h_input_tokens}`.
    // Clamped to the total so a malformed split can never bill more writes
    // than the record reports.
    let cache_write_1h = usage
        .get("cache_creation")
        .map(|split| u64_at(split, "ephemeral_1h_input_tokens"))
        .unwrap_or(0)
        .min(cache_write);

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
        .unwrap_or_else(Timestamp::now);

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
        cache_write_1h,
        logged_cost,
    })
}

fn u64_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn parse_ts(s: &str) -> Option<Timestamp> {
    s.parse().ok()
}

fn parse_date(s: &str) -> Option<Date> {
    Date::strptime("%Y-%m-%d", s.trim()).ok()
}

/// Instant rendered in the machine's timezone, for day/hour bucketing.
fn local(ts: Timestamp) -> ZonedDateTime {
    ts.to_zoned_datetime(jiff::tz::TimeZone::system())
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
        Group::Daily => local(r.ts).strftime("%Y-%m-%d").to_string(),
        Group::Weekly => {
            let iso = local(r.ts).date().iso_week_date();
            format!("{}-W{:02}", iso.year(), iso.week())
        }
        Group::Monthly => local(r.ts).strftime("%Y-%m").to_string(),
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
    let today = ZonedDateTime::now().date();
    let month_cost: f64 = records
        .iter()
        .filter(|r| {
            let d = local(r.ts).date();
            d.year() == today.year() && d.month() == today.month()
        })
        .map(|r| r.cost(mode))
        .sum();
    // `day()` is 1-based, so no zero guard is needed (the old one was dead code).
    month_cost / today.day() as f64 * today.days_in_month() as f64
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
    let now = Timestamp::now();
    let mut iter = records.iter();
    if let Some(first) = iter.next() {
        let mut start = floor_hour(first.ts);
        let mut tok = 0u64;
        let mut cost = 0.0;
        let mut latest = first.ts;
        let flush =
            |start: Timestamp, latest: Timestamp, tok: u64, cost: f64, now: Timestamp| -> Block {
                let end = start + SignedDuration::from_hours(BLOCK_HOURS);
                let active = now < end && now >= start;
                let (burn, proj) = if active {
                    let mins = minutes_between(start, now).max(1) as f64;
                    let burn = tok as f64 / mins;
                    // Extrapolating from a minutes-old sample over a 5-hour block
                    // produces headline numbers dominated by noise (one request at
                    // minute 1 projected 300× its cost). Only project once there is
                    // enough of the block observed for the rate to mean something.
                    const MIN_MINUTES_FOR_PROJECTION: f64 = 15.0;
                    let proj = if mins >= MIN_MINUTES_FOR_PROJECTION {
                        let remaining = minutes_between(now, end).max(0) as f64;
                        Some(cost + (cost / mins) * remaining)
                    } else {
                        None
                    };
                    (Some(burn), proj)
                } else {
                    (None, None)
                };
                let _ = latest;
                Block {
                    start: local(start).strftime("%Y-%m-%d %H:%M").to_string(),
                    end: local(end).strftime("%H:%M").to_string(),
                    tokens: tok,
                    cost_usd: cost,
                    active,
                    burn_per_min: burn,
                    projected_cost_usd: proj,
                }
            };
        for r in std::iter::once(first).chain(iter) {
            if r.ts >= start + SignedDuration::from_hours(BLOCK_HOURS) {
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

fn floor_hour(ts: Timestamp) -> Timestamp {
    let z = local(ts);
    z.with()
        .minute(0)
        .second(0)
        .subsec_nanosecond(0)
        .build()
        .map(|t| t.timestamp())
        .unwrap_or(ts)
}

/// Whole minutes from `from` to `to` (truncated toward zero).
fn minutes_between(from: Timestamp, to: Timestamp) -> i64 {
    to.duration_since(from).as_secs() / 60
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn print_table(rows: &[Row], total: &Row, opts: &Options, records: &[Record]) {
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
        // The caller already holds the records this needs. Re-collecting here
        // re-read every transcript on disk a second time for one number.
        println!(
            "  {} {}",
            "month-end forecast:".dimmed(),
            fmt_cost(month_forecast(records, opts.cost_mode)).yellow()
        );
    }
}

fn print_statusline(records: &[Record], mode: CostMode) {
    let now = Timestamp::now();
    let today = local(now).date();
    let mut cost = 0.0;
    let mut tokens = 0u64;
    for r in records.iter().filter(|r| local(r.ts).date() == today) {
        cost += r.cost(mode);
        tokens += r.tokens();
    }
    // Active-block burn rate.
    let win_start = now - SignedDuration::from_hours(BLOCK_HOURS);
    // Divide by the minutes actually observed, not the full 5-hour window: a
    // session 10 minutes old reported ~1/30th of its real rate, and disagreed
    // with the same number in `report_blocks`.
    let burn = records.iter().find(|r| r.ts >= win_start).map(|first| {
        let tok: u64 = records
            .iter()
            .filter(|r| r.ts >= win_start)
            .map(|r| r.tokens())
            .sum();
        let mins = minutes_between(first.ts, now).max(1) as f64;
        tok as f64 / mins
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
    if s.chars().count() > 20 {
        format!("{}…", s.chars().take(19).collect::<String>())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(model: &str, input: u64, cache_read: u64, logged: Option<f64>) -> Record {
        Record {
            ts: Timestamp::now(),
            model: model.to_string(),
            project: "proj".to_string(),
            session: "sess".to_string(),
            input,
            output: 0,
            cache_read,
            cache_write: 0,
            cache_write_1h: 0,
            logged_cost: logged,
        }
    }

    #[test]
    fn tokens_counts_every_category() {
        // Cache reads and writes are billed tokens; leaving them out of the total
        // understates real spend by the category that usually dominates it.
        let r = Record {
            cache_write: 4,
            output: 2,
            ..rec("claude-sonnet-4-6", 1, 3, None)
        };
        assert_eq!(r.tokens(), 10);
    }

    #[test]
    fn cost_modes_pick_logged_or_calculated() {
        // sonnet: input $3/1M, cache read $0.30/1M.
        // 1_000_000 input + 1_000_000 cache_read = 3.00 + 0.30.
        let r = rec("claude-sonnet-4-6", 1_000_000, 1_000_000, Some(9.99));
        assert!((r.cost(CostMode::Calculate) - 3.30).abs() < 1e-9);
        assert_eq!(r.cost(CostMode::Display), 9.99);
        // Auto prefers what the agent actually logged.
        assert_eq!(r.cost(CostMode::Auto), 9.99);

        let unlogged = rec("claude-sonnet-4-6", 1_000_000, 0, None);
        assert!((unlogged.cost(CostMode::Auto) - 3.00).abs() < 1e-9);
        // Display-only means "show nothing we did not receive", not "estimate".
        assert_eq!(unlogged.cost(CostMode::Display), 0.0);
    }

    #[test]
    fn unknown_model_costs_zero_rather_than_guessing() {
        let r = rec("some-local-llama", 1_000_000, 0, None);
        assert_eq!(r.cost(CostMode::Calculate), 0.0);
    }

    #[test]
    fn forecast_month_length_handles_leap_years_and_december() {
        assert_eq!(Date::constant(2026, 1, 1).days_in_month(), 31);
        assert_eq!(Date::constant(2026, 2, 1).days_in_month(), 28);
        assert_eq!(Date::constant(2024, 2, 1).days_in_month(), 29); // leap
        assert_eq!(Date::constant(2026, 4, 1).days_in_month(), 30);
        assert_eq!(Date::constant(2026, 12, 1).days_in_month(), 31); // year rollover path
    }

    #[test]
    fn month_forecast_extrapolates_from_days_elapsed() {
        // One record in the current month, priced deterministically.
        let r = rec("claude-sonnet-4-6", 1_000_000, 0, Some(10.0));
        let today = ZonedDateTime::now().date();
        let expected = 10.0 / today.day() as f64 * today.days_in_month() as f64;
        assert!((month_forecast(&[r], CostMode::Auto) - expected).abs() < 1e-9);
    }

    #[test]
    fn month_forecast_ignores_other_months() {
        let mut old = rec("claude-sonnet-4-6", 1_000_000, 0, Some(10.0));
        old.ts = Timestamp::now() - SignedDuration::from_hours(24 * 400);
        assert_eq!(month_forecast(&[old], CostMode::Auto), 0.0);
    }

    #[test]
    fn parse_helpers_accept_rfc3339_and_iso_dates_only() {
        assert!(parse_ts("2026-08-01T10:00:00Z").is_some());
        assert!(parse_ts("2026-08-01T10:00:00-03:00").is_some());
        assert!(parse_ts("not a time").is_none());
        assert_eq!(parse_date(" 2026-08-01 ").map(|d| d.day()), Some(1));
        assert!(parse_date("2026-13-01").is_none());
        assert!(parse_date("garbage").is_none());
    }

    #[test]
    fn totals_sum_every_record() {
        let rows = vec![
            rec("claude-sonnet-4-6", 10, 5, Some(1.0)),
            rec("claude-sonnet-4-6", 20, 0, Some(2.5)),
        ];
        let t = totals(&rows, CostMode::Auto);
        assert_eq!(t.input, 30);
        assert_eq!(t.cache_read, 5);
        assert_eq!(t.tokens, 35);
        assert!((t.cost_usd - 3.5).abs() < 1e-9);
    }

    #[test]
    fn floor_hour_drops_minutes_seconds_and_nanos() {
        let ts = local(Timestamp::now())
            .with()
            .minute(43)
            .second(21)
            .build()
            .unwrap()
            .timestamp();
        let floored = local(floor_hour(ts));
        assert_eq!(floored.minute(), 0);
        assert_eq!(floored.second(), 0);
        assert_eq!(floored.hour(), local(ts).hour());
    }

    #[test]
    fn duplicate_usage_lines_are_counted_once() {
        // Transcripts replay the same assistant message (resume, fork), and both
        // copies carry the same usage block. Counting both double-bills the user.
        let line = serde_json::json!({
            "timestamp": "2026-08-01T10:00:00Z",
            "requestId": "req-1",
            "message": {
                "id": "msg-1",
                "model": "claude-sonnet-4-6",
                "usage": { "input_tokens": 100, "output_tokens": 5 }
            }
        });
        let mut seen = HashSet::new();
        assert!(record_from_value(&line, "fallback", &mut seen).is_some());
        assert!(
            record_from_value(&line, "fallback", &mut seen).is_none(),
            "the same (message id, requestId) must not be billed twice"
        );
    }

    #[test]
    fn records_without_usage_are_skipped() {
        let mut seen = HashSet::new();
        let no_usage = serde_json::json!({ "message": { "model": "claude-sonnet-4-6" } });
        assert!(record_from_value(&no_usage, "f", &mut seen).is_none());
        // All-zero usage carries no information either.
        let zeros = serde_json::json!({
            "message": { "usage": { "input_tokens": 0, "output_tokens": 0 } }
        });
        assert!(record_from_value(&zeros, "f", &mut seen).is_none());
    }

    #[test]
    fn cache_fields_are_read_from_the_usage_block() {
        let mut seen = HashSet::new();
        let v = serde_json::json!({
            "message": {
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 2,
                    "cache_read_input_tokens": 30,
                    "cache_creation_input_tokens": 40
                }
            }
        });
        let r = record_from_value(&v, "f", &mut seen).expect("record");
        assert_eq!((r.cache_read, r.cache_write), (30, 40));
        assert_eq!(r.tokens(), 73);
    }

    #[test]
    fn one_hour_cache_writes_are_priced_at_their_own_rate() {
        let mut seen = HashSet::new();
        let v = serde_json::json!({
            "message": {
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 1_000_000,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 400_000,
                        "ephemeral_1h_input_tokens": 600_000
                    }
                }
            }
        });
        let r = record_from_value(&v, "f", &mut seen).expect("record");
        assert_eq!((r.cache_write, r.cache_write_1h), (1_000_000, 600_000));
        // Sonnet input is $3/M: 0.4M at 1.25x ($1.50) + 0.6M at 2x ($3.60).
        assert!((r.cost(CostMode::Calculate) - 5.10).abs() < 1e-9);
        // The split never adds tokens: it is a share of the same total.
        assert_eq!(r.tokens(), 1_000_000);
    }

    #[test]
    fn a_one_hour_split_larger_than_the_total_is_clamped() {
        let mut seen = HashSet::new();
        let v = serde_json::json!({
            "message": {
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 1,
                    "cache_creation_input_tokens": 10,
                    "cache_creation": { "ephemeral_1h_input_tokens": 999 }
                }
            }
        });
        let r = record_from_value(&v, "f", &mut seen).expect("record");
        assert_eq!(r.cache_write_1h, 10);
    }
}
