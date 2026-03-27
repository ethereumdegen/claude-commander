#![allow(dead_code)]

use std::collections::HashMap;
use std::process::Command;

use serde::Deserialize;

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

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
