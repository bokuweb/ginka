//! What a session cost, and how close the user is to their plan's ceiling.
//!
//! Two separate questions, and the second is the one asked mid-task. Token
//! totals do not answer "how close am I to the wall" — a rate-limit window
//! does (`docs/roadmap.md` §3.3 N12). Costs come from a rate table that is
//! fetched, cached and allowed to go stale, and the summary states its own
//! confidence rather than quietly pricing an unknown model at zero (§3.3 N13).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
/// The plan windows are wire types now — a client draws them — and are kept
/// reachable from here because this is where they were first written.
pub use ginka_protocol::model::{PlanUsage, PlanWindow};
use ginka_protocol::provider::ProviderKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// How long a cached rate table is considered current. Rates move rarely, and
/// a day-old table keeps the page working offline.
pub const RATES_LIFETIME_SECS: i64 = 24 * 3_600;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenTotals {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// Everything that went *into* the model. Cache hits are cheaper input,
    /// not a different thing.
    pub fn billable_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEvent {
    pub session: String,
    pub provider: ProviderKind,
    pub model: String,
    pub tokens: TokenTotals,
    /// Unix seconds.
    pub at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelRate {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: f64,
    pub cache_write_per_million: f64,
}

/// Prices per model, cached beside the database.
///
/// The table is data we fetch, not data we maintain: hardcoded prices are
/// wrong within weeks of a vendor's next announcement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateTable {
    /// When this copy was fetched, in unix seconds.
    pub fetched_at: i64,
    rates: BTreeMap<String, ModelRate>,
}

impl RateTable {
    pub fn empty(fetched_at: i64) -> Self {
        Self {
            fetched_at,
            rates: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, model: &str, rate: ModelRate) {
        self.rates.insert(normalize_model(model), rate);
    }

    pub fn is_empty(&self) -> bool {
        self.rates.is_empty()
    }

    /// Past its lifetime the table is still used — an offline user gets
    /// yesterday's prices rather than none — but the page can say so.
    pub fn is_stale(&self, now: i64) -> bool {
        now - self.fetched_at > RATES_LIFETIME_SECS
    }

    /// `None` when the model is not in the table, which is a different answer
    /// from "it was free".
    pub fn cost_of(&self, model: &str, tokens: &TokenTotals) -> Option<f64> {
        let rate = self.rate_for(model)?;
        let million = 1_000_000.0;
        Some(
            tokens.input as f64 / million * rate.input_per_million
                + tokens.output as f64 / million * rate.output_per_million
                + tokens.cache_read as f64 / million * rate.cache_read_per_million
                + tokens.cache_write as f64 / million * rate.cache_write_per_million,
        )
    }

    fn rate_for(&self, model: &str) -> Option<&ModelRate> {
        let key = normalize_model(model);
        if let Some(rate) = self.rates.get(&key) {
            return Some(rate);
        }
        // Vendors append a dated build id to the same model. The longest
        // stored key the id starts with is the right one: `claude-haiku-4-5`
        // must win over `claude-haiku` when both are listed.
        self.rates
            .iter()
            .filter(|(stored, _)| key.starts_with(stored.as_str()))
            .max_by_key(|(stored, _)| stored.len())
            .map(|(_, rate)| rate)
    }

    /// Read the cached table. A missing or unreadable cache is a miss, never
    /// an error: the usage page must open whatever state the cache is in.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        Ok(match serde_json::from_str(&text) {
            Ok(table) => Some(table),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "rate cache is unreadable; refetching");
                None
            }
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(self)?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// How much of a summary's cost we actually know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CostQuality {
    /// Every model in the range was priced.
    Priced,
    /// Some were not, and these are they.
    Partial { unpriced_models: Vec<String> },
    /// No rate table at all.
    Unpriced,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DaySlice {
    /// `YYYY-MM-DD`, in UTC. Days are a reporting bucket, not a local clock.
    pub date: String,
    pub tokens: TokenTotals,
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelSlice {
    pub model: String,
    pub tokens: TokenTotals,
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageSummary {
    pub tokens: TokenTotals,
    /// `None` only when nothing could be priced at all.
    pub cost: Option<f64>,
    pub quality: CostQuality,
    pub by_day: Vec<DaySlice>,
    pub by_model: Vec<ModelSlice>,
}

/// Roll events up for the usage page.
pub fn summarize(events: &[UsageEvent], rates: Option<&RateTable>) -> UsageSummary {
    let mut totals = TokenTotals::default();
    let mut days: BTreeMap<String, TokenTotals> = BTreeMap::new();
    let mut models: BTreeMap<String, TokenTotals> = BTreeMap::new();

    for event in events {
        totals.add(&event.tokens);
        days.entry(day_of(event.at)).or_default().add(&event.tokens);
        models
            .entry(event.model.clone())
            .or_default()
            .add(&event.tokens);
    }

    let Some(rates) = rates else {
        return UsageSummary {
            tokens: totals,
            cost: None,
            quality: CostQuality::Unpriced,
            by_day: days
                .into_iter()
                .map(|(date, tokens)| DaySlice {
                    date,
                    tokens,
                    cost: None,
                })
                .collect(),
            by_model: models
                .into_iter()
                .map(|(model, tokens)| ModelSlice {
                    model,
                    tokens,
                    cost: None,
                })
                .collect(),
        };
    };

    let mut cost = 0.0;
    let mut unpriced = Vec::new();
    let by_model: Vec<ModelSlice> = models
        .into_iter()
        .map(|(model, tokens)| {
            let priced = rates.cost_of(&model, &tokens);
            match priced {
                Some(amount) => cost += amount,
                None => unpriced.push(model.clone()),
            }
            ModelSlice {
                model,
                tokens,
                cost: priced,
            }
        })
        .collect();

    // A day's cost is only meaningful when everything in it was priced, so it
    // is left out rather than under-reported.
    let by_day = days
        .into_iter()
        .map(|(date, tokens)| DaySlice {
            date,
            tokens,
            cost: None,
        })
        .collect();

    let quality = if unpriced.is_empty() {
        CostQuality::Priced
    } else {
        CostQuality::Partial {
            unpriced_models: unpriced,
        }
    };

    UsageSummary {
        tokens: totals,
        cost: Some(cost),
        quality,
        by_day,
        by_model,
    }
}

/// Vendors prefix their own name and suffix a build date onto the same model.
fn normalize_model(model: &str) -> String {
    model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .trim()
        .to_ascii_lowercase()
}

fn day_of(timestamp: i64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}
