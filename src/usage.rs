#![allow(dead_code)]

use std::collections::HashMap;
use std::process::Command;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, BarGroup, Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use serde::Deserialize;

use crate::theme;

// ── Data structures from ~/.claude/stats-cache.json ──

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StatsCache {
    #[serde(default)]
    pub daily_activity: Vec<DailyActivity>,
    #[serde(default)]
    pub model_usage: HashMap<String, ModelUsage>,
    #[serde(default)]
    pub last_computed_date: String,
    #[serde(default)]
    pub total_sessions: u64,
    #[serde(default)]
    pub total_messages: u64,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DailyActivity {
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub message_count: u64,
    #[serde(default)]
    pub session_count: u64,
    #[serde(default)]
    pub tool_call_count: u64,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default, rename = "costUSD")]
    pub cost_usd: f64,
}

// ── Live usage from API ──

#[derive(Debug, Deserialize, Default)]
pub struct LiveUsage {
    #[serde(default, deserialize_with = "deserialize_bucket")]
    pub five_hour: UsageBucket,
    #[serde(default, deserialize_with = "deserialize_bucket")]
    pub seven_day: UsageBucket,
    #[serde(default, deserialize_with = "deserialize_bucket")]
    pub seven_day_sonnet: UsageBucket,
    #[serde(default, deserialize_with = "deserialize_bucket")]
    pub seven_day_opus: UsageBucket,
}

/// Deserialize a UsageBucket that may be null in the API response.
fn deserialize_bucket<'de, D>(deserializer: D) -> Result<UsageBucket, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<UsageBucket>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct UsageBucket {
    #[serde(default)]
    pub utilization: f64,
    #[serde(default)]
    pub resets_at: String,
}

// ── Fetching ──

pub fn load_stats_cache() -> StatsCache {
    let home = dirs::home_dir().unwrap_or_default();
    let path = home.join(".claude").join("stats-cache.json");
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => StatsCache::default(),
    }
}

pub fn fetch_live_usage() -> LiveUsage {
    let home = dirs::home_dir().unwrap_or_default();
    let creds_path = home.join(".claude").join(".credentials.json");

    let token = match std::fs::read_to_string(&creds_path) {
        Ok(data) => {
            let val: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
            val.get("claudeAiOauth")
                .and_then(|o| o.get("accessToken"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string()
        }
        Err(_) => return LiveUsage::default(),
    };

    if token.is_empty() {
        return LiveUsage::default();
    }

    let output = Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "5",
            "-H",
            &format!("Authorization: Bearer {}", token),
            "-H",
            "anthropic-beta: oauth-2025-04-20",
            "https://api.anthropic.com/api/oauth/usage",
        ])
        .output();

    match output {
        Ok(o) => {
            let body = String::from_utf8_lossy(&o.stdout);
            serde_json::from_str(&body).unwrap_or_default()
        }
        Err(_) => LiveUsage::default(),
    }
}

// ── Rendering ──

pub fn render_usage(f: &mut Frame, area: Rect, stats: &StatsCache, live: &LiveUsage) {
    let outer = Block::default()
        .title("  Token Usage Dashboard  ")
        .title_style(Style::default().fg(theme::CYAN()).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_FOCUSED()))
        .style(Style::default().bg(theme::bg_primary()));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // utilization bars
            Constraint::Length(3),  // spacer + heading
            Constraint::Min(8),    // daily chart
            Constraint::Length(6), // model breakdown
        ])
        .split(inner);

    // ── Utilization gauges ──
    render_utilization(f, chunks[0], live);

    // ── Heading ──
    let heading = Paragraph::new(Line::from(vec![
        Span::styled(
            " Daily Activity (last 14 days) ",
            Style::default()
                .fg(theme::text_primary())
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .style(Style::default().bg(theme::bg_primary()));
    f.render_widget(heading, chunks[1]);

    // ── Daily bar chart ──
    render_daily_chart(f, chunks[2], stats);

    // ── Model breakdown ──
    render_model_breakdown(f, chunks[3], stats);
}

fn render_utilization(f: &mut Frame, area: Rect, live: &LiveUsage) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(area);

    let buckets = [
        ("5h Window", &live.five_hour),
        ("7d Overall", &live.seven_day),
        ("7d Sonnet", &live.seven_day_sonnet),
        ("7d Opus", &live.seven_day_opus),
    ];

    for (i, (label, bucket)) in buckets.iter().enumerate() {
        // API returns utilization as a percentage (0–100), not a fraction
        let pct = bucket.utilization.min(100.0);
        let frac = pct / 100.0;
        let color = theme::utilization_color(frac);
        let filled = ((cols[i].width.saturating_sub(2)) as f64 * frac) as u16;

        let bar_line = format!(
            "{} {:>5.1}%",
            "█".repeat(filled as usize)
                + &"░".repeat((cols[i].width.saturating_sub(2) - filled).saturating_sub(8) as usize),
            pct
        );

        let widget = Paragraph::new(vec![
            Line::from(Span::styled(
                format!(" {}", label),
                Style::default()
                    .fg(theme::text_secondary())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(bar_line, Style::default().fg(color))),
        ])
        .block(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(theme::BORDER_NORMAL())),
        )
        .style(Style::default().bg(theme::bg_secondary()));

        f.render_widget(widget, cols[i]);
    }
}

fn render_daily_chart(f: &mut Frame, area: Rect, stats: &StatsCache) {
    let bar_colors = theme::BAR_COLORS();

    let days: Vec<&DailyActivity> = stats
        .daily_activity
        .iter()
        .rev()
        .take(14)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if days.is_empty() {
        let empty = Paragraph::new("  No daily activity data found")
            .style(Style::default().fg(theme::text_muted()).bg(theme::bg_primary()));
        f.render_widget(empty, area);
        return;
    }

    let bars: Vec<Bar> = days
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let label = if d.date.len() >= 10 {
                &d.date[5..10] // MM-DD
            } else {
                &d.date
            };
            Bar::default()
                .value(d.message_count)
                .label(Line::from(label.to_string()))
                .style(Style::default().fg(bar_colors[i % bar_colors.len()]))
        })
        .collect();

    let chart = BarChart::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_NORMAL()))
                .style(Style::default().bg(theme::bg_primary())),
        )
        .data(BarGroup::default().bars(&bars))
        .bar_width(3)
        .bar_gap(1)
        .direction(Direction::Vertical)
        .style(Style::default().bg(theme::bg_primary()));

    f.render_widget(chart, area);
}

fn render_model_breakdown(f: &mut Frame, area: Rect, stats: &StatsCache) {
    let bar_colors = theme::BAR_COLORS();

    let mut lines = vec![Line::from(Span::styled(
        " Model Token Breakdown",
        Style::default()
            .fg(theme::text_primary())
            .add_modifier(Modifier::BOLD),
    ))];

    if stats.model_usage.is_empty() {
        lines.push(Line::from(Span::styled(
            "   No model usage data",
            Style::default().fg(theme::text_muted()),
        )));
    } else {
        let mut models: Vec<_> = stats.model_usage.iter().collect();
        models.sort_by(|a, b| {
            (b.1.input_tokens + b.1.output_tokens).cmp(&(a.1.input_tokens + a.1.output_tokens))
        });

        for (i, (name, usage)) in models.iter().take(4).enumerate() {
            let total = usage.input_tokens + usage.output_tokens;
            let color = bar_colors[i % bar_colors.len()];
            let short_name = name
                .split('/')
                .last()
                .unwrap_or(name)
                .chars()
                .take(25)
                .collect::<String>();

            lines.push(Line::from(vec![
                Span::styled(format!("   ● {:<25}", short_name), Style::default().fg(color)),
                Span::styled(
                    format!(
                        " in:{:>8}  out:{:>8}  cache:{:>8}  total:{:>9}",
                        format_tokens(usage.input_tokens),
                        format_tokens(usage.output_tokens),
                        format_tokens(usage.cache_read_input_tokens),
                        format_tokens(total),
                    ),
                    Style::default().fg(theme::text_secondary()),
                ),
            ]));
        }
    }

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(theme::bg_primary()));
    f.render_widget(widget, area);
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
