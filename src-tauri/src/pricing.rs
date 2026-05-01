//! Per-token pricing for Codex (OpenAI) and Claude (Anthropic) models.
//!
//! Ported from Swift `CostUsageScanner.Pricing` in the macOS app.
//! Rates are USD per token (not per million).
//!
//! Claude sonnet-4-5 / sonnet-4-6 / sonnet-4-20250514 use tiered pricing:
//! first 200K input tokens at base rate, above threshold at 2x rate.

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct CodexModel {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct ClaudeModel {
    pub input: f64,
    pub output: f64,
    pub cache_creation: f64,
    pub cache_read: f64,
    pub threshold: Option<i64>,
    pub input_above: Option<f64>,
    pub output_above: Option<f64>,
    pub cache_creation_above: Option<f64>,
    pub cache_read_above: Option<f64>,
}

static CODEX_MODELS: Lazy<HashMap<&'static str, CodexModel>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert(
        "gpt-5",
        CodexModel {
            input: 1.25e-6,
            output: 1e-5,
            cache_read: Some(1.25e-7),
        },
    );
    m.insert(
        "gpt-5-codex",
        CodexModel {
            input: 1.25e-6,
            output: 1e-5,
            cache_read: Some(1.25e-7),
        },
    );
    m.insert(
        "gpt-5-mini",
        CodexModel {
            input: 2.5e-7,
            output: 2e-6,
            cache_read: Some(2.5e-8),
        },
    );
    m.insert(
        "gpt-5-nano",
        CodexModel {
            input: 5e-8,
            output: 4e-7,
            cache_read: Some(5e-9),
        },
    );
    m.insert(
        "gpt-5-pro",
        CodexModel {
            input: 1.5e-5,
            output: 1.2e-4,
            cache_read: None,
        },
    );
    m.insert(
        "gpt-5.1",
        CodexModel {
            input: 1.25e-6,
            output: 1e-5,
            cache_read: Some(1.25e-7),
        },
    );
    m.insert(
        "gpt-5.1-codex",
        CodexModel {
            input: 1.25e-6,
            output: 1e-5,
            cache_read: Some(1.25e-7),
        },
    );
    m.insert(
        "gpt-5.1-codex-max",
        CodexModel {
            input: 1.25e-6,
            output: 1e-5,
            cache_read: Some(1.25e-7),
        },
    );
    m.insert(
        "gpt-5.1-codex-mini",
        CodexModel {
            input: 2.5e-7,
            output: 2e-6,
            cache_read: Some(2.5e-8),
        },
    );
    m.insert(
        "gpt-5.2",
        CodexModel {
            input: 1.75e-6,
            output: 1.4e-5,
            cache_read: Some(1.75e-7),
        },
    );
    m.insert(
        "gpt-5.2-codex",
        CodexModel {
            input: 1.75e-6,
            output: 1.4e-5,
            cache_read: Some(1.75e-7),
        },
    );
    m.insert(
        "gpt-5.2-pro",
        CodexModel {
            input: 2.1e-5,
            output: 1.68e-4,
            cache_read: None,
        },
    );
    m.insert(
        "gpt-5.3-codex",
        CodexModel {
            input: 1.75e-6,
            output: 1.4e-5,
            cache_read: Some(1.75e-7),
        },
    );
    m.insert(
        "gpt-5.3-codex-spark",
        CodexModel {
            input: 0.0,
            output: 0.0,
            cache_read: Some(0.0),
        },
    );
    m.insert(
        "gpt-5.4",
        CodexModel {
            input: 2.5e-6,
            output: 1.5e-5,
            cache_read: Some(2.5e-7),
        },
    );
    m.insert(
        "gpt-5.4-mini",
        CodexModel {
            input: 7.5e-7,
            output: 4.5e-6,
            cache_read: Some(7.5e-8),
        },
    );
    m.insert(
        "gpt-5.4-nano",
        CodexModel {
            input: 2e-7,
            output: 1.25e-6,
            cache_read: Some(2e-8),
        },
    );
    m.insert(
        "gpt-5.4-pro",
        CodexModel {
            input: 3e-5,
            output: 1.8e-4,
            cache_read: None,
        },
    );
    m
});

static CLAUDE_MODELS: Lazy<HashMap<&'static str, ClaudeModel>> = Lazy::new(|| {
    let mut m = HashMap::new();
    // Haiku 4.5
    let haiku_4_5 = ClaudeModel {
        input: 1e-6,
        output: 5e-6,
        cache_creation: 1.25e-6,
        cache_read: 1e-7,
        threshold: None,
        input_above: None,
        output_above: None,
        cache_creation_above: None,
        cache_read_above: None,
    };
    m.insert("claude-haiku-4-5", haiku_4_5);
    m.insert("claude-haiku-4-5-20251001", haiku_4_5);

    // Opus 4.5 / 4.6
    let opus_45_46 = ClaudeModel {
        input: 5e-6,
        output: 2.5e-5,
        cache_creation: 6.25e-6,
        cache_read: 5e-7,
        threshold: None,
        input_above: None,
        output_above: None,
        cache_creation_above: None,
        cache_read_above: None,
    };
    m.insert("claude-opus-4-5", opus_45_46);
    m.insert("claude-opus-4-5-20251101", opus_45_46);
    m.insert("claude-opus-4-6", opus_45_46);
    m.insert("claude-opus-4-6-20260205", opus_45_46);
    // Opus 4.7 — same pricing tier as 4.5 / 4.6
    m.insert("claude-opus-4-7", opus_45_46);

    // Sonnet 4.5 / 4.6 — tiered above 200K
    let sonnet_tiered = ClaudeModel {
        input: 3e-6,
        output: 1.5e-5,
        cache_creation: 3.75e-6,
        cache_read: 3e-7,
        threshold: Some(200_000),
        input_above: Some(6e-6),
        output_above: Some(2.25e-5),
        cache_creation_above: Some(7.5e-6),
        cache_read_above: Some(6e-7),
    };
    m.insert("claude-sonnet-4-5", sonnet_tiered);
    m.insert("claude-sonnet-4-5-20250929", sonnet_tiered);
    m.insert("claude-sonnet-4-6", sonnet_tiered);
    m.insert("claude-sonnet-4-20250514", sonnet_tiered);

    // Opus 4 / 4.1 (legacy)
    let opus_4 = ClaudeModel {
        input: 1.5e-5,
        output: 7.5e-5,
        cache_creation: 1.875e-5,
        cache_read: 1.5e-6,
        threshold: None,
        input_above: None,
        output_above: None,
        cache_creation_above: None,
        cache_read_above: None,
    };
    m.insert("claude-opus-4-20250514", opus_4);
    m.insert("claude-opus-4-1", opus_4);

    m
});

static CODEX_DATED_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-\d{4}-\d{2}-\d{2}$").unwrap());
static CLAUDE_DATED_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-\d{8}$").unwrap());
static CLAUDE_BEDROCK_VER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-v\d+:\d+$").unwrap());

pub fn normalize_codex_model(raw: &str) -> String {
    let trimmed = raw.trim();
    let trimmed = trimmed.strip_prefix("openai/").unwrap_or(trimmed);
    if CODEX_MODELS.contains_key(trimmed) {
        return trimmed.to_string();
    }
    if let Some(m) = CODEX_DATED_RE.find(trimmed) {
        let base = &trimmed[..m.start()];
        if CODEX_MODELS.contains_key(base) {
            return base.to_string();
        }
    }
    trimmed.to_string()
}

pub fn normalize_claude_model(raw: &str) -> String {
    let mut trimmed = raw.trim().to_string();
    if let Some(rest) = trimmed.strip_prefix("anthropic.") {
        trimmed = rest.to_string();
    }
    // Strip Bedrock vendor prefixes like "us.anthropic.claude-sonnet-4-5"
    if let Some(last_dot) = trimmed.rfind('.') {
        if trimmed.contains("claude-") {
            let tail = &trimmed[last_dot + 1..];
            if tail.starts_with("claude-") {
                trimmed = tail.to_string();
            }
        }
    }
    // Strip Bedrock version suffix: "-v1:0"
    if let Some(m) = CLAUDE_BEDROCK_VER_RE.find(&trimmed) {
        trimmed = trimmed[..m.start()].to_string();
    }
    // If dated form ("-YYYYMMDD") matches a base in the table, prefer base
    if let Some(m) = CLAUDE_DATED_RE.find(&trimmed) {
        let base = &trimmed[..m.start()];
        if CLAUDE_MODELS.contains_key(base) {
            return base.to_string();
        }
    }
    trimmed
}

pub fn codex_cost_usd(
    model: &str,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
) -> Option<f64> {
    let key = normalize_codex_model(model);
    let p = CODEX_MODELS.get(key.as_str())?;
    let cached = cached_input_tokens.max(0).min(input_tokens.max(0));
    let non_cached = (input_tokens - cached).max(0);
    let cached_rate = p.cache_read.unwrap_or(p.input);
    Some(
        non_cached as f64 * p.input
            + cached as f64 * cached_rate
            + output_tokens.max(0) as f64 * p.output,
    )
}

pub fn claude_cost_usd(
    model: &str,
    input_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    output_tokens: i64,
) -> Option<f64> {
    let key = normalize_claude_model(model);
    let p = CLAUDE_MODELS.get(key.as_str())?;

    fn tiered(tokens: i64, base: f64, above: Option<f64>, threshold: Option<i64>) -> f64 {
        let tokens = tokens.max(0);
        match (threshold, above) {
            (Some(t), Some(a)) => {
                let below = tokens.min(t);
                let over = (tokens - t).max(0);
                below as f64 * base + over as f64 * a
            }
            _ => tokens as f64 * base,
        }
    }

    Some(
        tiered(input_tokens, p.input, p.input_above, p.threshold)
            + tiered(
                cache_read_input_tokens,
                p.cache_read,
                p.cache_read_above,
                p.threshold,
            )
            + tiered(
                cache_creation_input_tokens,
                p.cache_creation,
                p.cache_creation_above,
                p.threshold,
            )
            + tiered(output_tokens, p.output, p.output_above, p.threshold),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_codex_dated_suffix_strips_to_base() {
        assert_eq!(
            normalize_codex_model("gpt-5-codex-2025-11-15"),
            "gpt-5-codex"
        );
        assert_eq!(normalize_codex_model("openai/gpt-5.4"), "gpt-5.4");
        assert_eq!(normalize_codex_model("unknown-model"), "unknown-model");
    }

    #[test]
    fn normalize_claude_bedrock_prefix_strips() {
        assert_eq!(
            normalize_claude_model("us.anthropic.claude-sonnet-4-5-v1:0"),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            normalize_claude_model("claude-sonnet-4-5-20250929"),
            "claude-sonnet-4-5"
        );
    }

    #[test]
    fn codex_cost_basic_gpt5() {
        // 1M input @ $1.25/M, 0 cache, 100K output @ $10/M  = $1.25 + $1.00 = $2.25
        let c = codex_cost_usd("gpt-5", 1_000_000, 0, 100_000).unwrap();
        assert!((c - 2.25).abs() < 1e-9, "expected 2.25, got {}", c);
    }

    #[test]
    fn codex_cost_cached_input_discounted() {
        // 1M input, 500K of it cached (10x cheaper), 0 output
        // 500K @ $1.25/M + 500K @ $0.125/M = $0.625 + $0.0625 = $0.6875
        let c = codex_cost_usd("gpt-5", 1_000_000, 500_000, 0).unwrap();
        assert!((c - 0.6875).abs() < 1e-9, "expected 0.6875, got {}", c);
    }

    #[test]
    fn claude_cost_sonnet_tier_boundary() {
        // 200K input (at threshold) @ $3/M = $0.60
        let c = claude_cost_usd("claude-sonnet-4-6", 200_000, 0, 0, 0).unwrap();
        assert!((c - 0.60).abs() < 1e-9, "expected 0.60, got {}", c);

        // 300K input: 200K @ $3/M + 100K @ $6/M = $0.60 + $0.60 = $1.20
        let c = claude_cost_usd("claude-sonnet-4-6", 300_000, 0, 0, 0).unwrap();
        assert!((c - 1.20).abs() < 1e-9, "expected 1.20, got {}", c);
    }

    #[test]
    fn claude_cost_haiku_no_tier() {
        // Haiku has no threshold — 1M input @ $1/M = $1.00
        let c = claude_cost_usd("claude-haiku-4-5", 1_000_000, 0, 0, 0).unwrap();
        assert!((c - 1.00).abs() < 1e-9, "expected 1.00, got {}", c);
    }

    #[test]
    fn claude_cost_opus_4_7_priced_like_4_5_4_6() {
        // Opus 4.7 was missing from the table in v0.2.11, causing per-row Cost
        // to render "—" and the 7-day chart / Provider quota bar to collapse
        // to zero. Same per-token rates as Opus 4.5 / 4.6.
        // 1M input @ $5/M + 100K output @ $25/M = $5.00 + $2.50 = $7.50
        let c = claude_cost_usd("claude-opus-4-7", 1_000_000, 0, 0, 100_000).unwrap();
        assert!((c - 7.50).abs() < 1e-9, "expected 7.50, got {}", c);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(codex_cost_usd("gpt-42-unicorn", 1000, 0, 0).is_none());
        assert!(claude_cost_usd("claude-opus-99", 1000, 0, 0, 0).is_none());
    }
}
