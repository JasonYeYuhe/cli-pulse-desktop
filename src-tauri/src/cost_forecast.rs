//! v0.5.0 — month-end cost forecast via linear regression on daily
//! cost. Pure Rust port of `CostForecastEngine.swift` from the Mac
//! sibling app (`CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/
//! CostForecastEngine.swift` — v1.12.0 / iter21).
//!
//! Algorithm parity is the load-bearing requirement here. A paired
//! account viewed on Mac and on this desktop must show the same
//! forecast number for the same input. Float-tolerance ±0.01 USD
//! is acceptable; ordering of arithmetic operations is preserved
//! one-for-one against the Swift source.
//!
//! Critical edge case from the Swift iter21 hotfix (Sentry issue
//! 7450581409): on the LAST day of the month, `remaining_days == 0`,
//! and the original Swift extrapolation used a closed range
//! `(dayOfMonth + 1)...daysInMonth` which is invalid (e.g. `31...30`
//! traps `EXC_BREAKPOINT`). Rust would not panic, but the equivalent
//! `(day + 1..=days)` would yield an empty range and silently produce
//! a flat-final-day projection that's CORRECT but only because of
//! Rust's range semantics. The explicit `if remaining_days > 0` guard
//! below documents the intent and matches Swift's fix verbatim.
//!
//! Inputs are `supabase::DailyUsageRow`s, which arrive grouped by
//! (date, provider, model). We sum cost across all rows sharing a
//! `metric_date` to get per-day cost, then run linear regression on
//! that series.

use chrono::{Datelike, NaiveDate};
use serde::Serialize;
use std::collections::BTreeMap;

use crate::supabase::DailyUsageRow;

/// Forecast result. All cost values in USD. Field names match the
/// Swift struct's serialized form so frontend code can be ported
/// from Mac with zero translation work.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CostForecast {
    /// Predicted total cost for the current month. Clamped never to
    /// be less than `actual_to_date` — the prediction can't show
    /// "you'll spend less than you already have."
    pub predicted_month_total: f64,
    /// Lower bound of the 1-stddev interval, clamped at
    /// `actual_to_date` (same reasoning as `predicted_month_total`).
    pub lower_bound: f64,
    /// Upper bound of the 1-stddev interval. Not clamped.
    pub upper_bound: f64,
    /// Actual cost summed for days 1..=current_day_of_month.
    pub actual_to_date: f64,
    /// Number of data points the forecast was built from. Always
    /// equals `current_day_of_month` after the dense-fill — missing
    /// days count as $0 cost. (Mirrors Swift's behavior.)
    pub data_point_count: usize,
    /// 1-based day-of-month of the reference date.
    pub current_day_of_month: u32,
    /// Total days in the month of the reference date.
    pub days_in_month: u32,
    /// `true` when there's enough data for a meaningful prediction.
    /// `data_point_count >= 3 && actual_to_date > 0`. Frontend uses
    /// this to render an amber "not enough data yet" hint instead of
    /// a confident-looking number.
    pub is_reliable: bool,
}

/// Generate a cost forecast from per-(date,provider,model) usage rows.
///
/// `reference_date` is the calendar date we're forecasting from
/// (defaults to `chrono::Local::now().date_naive()` at the call
/// site — kept as a parameter so tests can pin specific dates).
///
/// Returns `Some(CostForecast)` even when `daily` is empty — Swift
/// returns a zero forecast with `is_reliable: false` rather than
/// `nil`. Match that exactly so the UI doesn't have a separate
/// "no data" branch from "unreliable forecast" branch.
///
/// The single `None` return path is for invalid `reference_date`
/// inputs that don't have a determinable month length — chrono
/// makes those impossible (every NaiveDate has a valid month), so
/// in practice the function always returns `Some`. The `Option`
/// return is preserved for API parity with Swift's `nil` guard.
pub fn forecast_from_daily(
    daily: &[DailyUsageRow],
    reference_date: NaiveDate,
) -> Option<CostForecast> {
    let day_of_month = reference_date.day();
    let days_in_month = days_in_month(reference_date)?;

    // Per-day cost map. Aggregation parity with Swift's
    // `aggregateCostByDate`.
    let cost_by_date: BTreeMap<String, f64> = aggregate_cost_by_date(daily);

    // Build dense series 1..=day_of_month. Missing days = $0.
    let mut points: Vec<(f64, f64)> = Vec::with_capacity(day_of_month as usize);
    let mut actual_to_date = 0.0_f64;
    for day in 1..=day_of_month {
        let key = format!(
            "{:04}-{:02}-{:02}",
            reference_date.year(),
            reference_date.month(),
            day
        );
        let cost = cost_by_date.get(&key).copied().unwrap_or(0.0);
        actual_to_date += cost;
        points.push((day as f64, cost));
    }

    let is_reliable = points.len() >= 3 && actual_to_date > 0.0;

    let avg_daily_cost = actual_to_date / day_of_month as f64;
    let simple_projection = avg_daily_cost * days_in_month as f64;

    let regression = linear_regression(&points);
    let remaining_days = days_in_month.saturating_sub(day_of_month);

    // iter21 hotfix parity: on the last day of the month skip the
    // extrapolation entirely. Without this guard, an empty range
    // would silently fall through, but the explicit branch
    // documents intent.
    let mut projected = actual_to_date;
    if remaining_days > 0 {
        for day in (day_of_month + 1)..=days_in_month {
            let predicted = regression.slope * day as f64 + regression.intercept;
            // Don't let predicted daily cost go negative — a
            // downward-trending series (e.g. weekly cooldown) can
            // produce negative slopes that would otherwise subtract
            // from the projected total.
            projected += predicted.max(0.0);
        }
    }

    // Blend: weight regression more when we have more data.
    // 14 days = full regression weight (capped at 0.8 to keep some
    // simple-projection contribution even on long series).
    let regression_weight = (points.len() as f64 / 14.0).min(0.8);
    let blended = projected * regression_weight + simple_projection * (1.0 - regression_weight);

    // Standard error → 1-stddev confidence interval.
    let residuals: Vec<f64> = points
        .iter()
        .map(|(x, y)| y - (regression.slope * x + regression.intercept))
        .collect();
    let std_dev = standard_deviation(&residuals);
    // Margin scales with sqrt(remaining days) — uncertainty
    // accumulates over future days.
    let margin_of_error = std_dev * (remaining_days as f64).sqrt();

    let lower_bound = (blended - margin_of_error).max(actual_to_date);
    let upper_bound = blended + margin_of_error;

    Some(CostForecast {
        predicted_month_total: blended.max(actual_to_date),
        lower_bound,
        upper_bound,
        actual_to_date,
        data_point_count: points.len(),
        current_day_of_month: day_of_month,
        days_in_month,
        is_reliable,
    })
}

fn aggregate_cost_by_date(rows: &[DailyUsageRow]) -> BTreeMap<String, f64> {
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for row in rows {
        *out.entry(row.metric_date.clone()).or_insert(0.0) += row.cost;
    }
    out
}

/// Slope/intercept of the least-squares fit through `points`.
/// Returns `(0, mean(y))` when n <= 1 (no slope determinable) or
/// when the variance of x is zero (all points share an x value —
/// shouldn't happen for our 1..n series but guarded anyway).
struct Regression {
    slope: f64,
    intercept: f64,
}

fn linear_regression(points: &[(f64, f64)]) -> Regression {
    let n = points.len() as f64;
    if n <= 1.0 {
        let y = points.first().map(|p| p.1).unwrap_or(0.0);
        return Regression {
            slope: 0.0,
            intercept: y,
        };
    }
    let sum_x: f64 = points.iter().map(|p| p.0).sum();
    let sum_y: f64 = points.iter().map(|p| p.1).sum();
    let sum_xy: f64 = points.iter().map(|p| p.0 * p.1).sum();
    let sum_x2: f64 = points.iter().map(|p| p.0 * p.0).sum();

    let denominator = n * sum_x2 - sum_x * sum_x;
    if denominator.abs() < 1e-10 {
        return Regression {
            slope: 0.0,
            intercept: sum_y / n,
        };
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denominator;
    let intercept = (sum_y - slope * sum_x) / n;
    Regression { slope, intercept }
}

/// Sample standard deviation (Bessel-corrected, n-1 denominator).
/// Returns 0 for n <= 1, matching Swift's guard.
fn standard_deviation(values: &[f64]) -> f64 {
    if values.len() <= 1 {
        return 0.0;
    }
    let n = values.len() as f64;
    let mean: f64 = values.iter().sum::<f64>() / n;
    let variance: f64 = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    variance.sqrt()
}

/// Number of days in the calendar month of `date`. chrono doesn't
/// expose this directly; compute via "first of next month minus 1".
fn days_in_month(date: NaiveDate) -> Option<u32> {
    let (y, m) = (date.year(), date.month());
    let (next_y, next_m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let first_next = NaiveDate::from_ymd_opt(next_y, next_m, 1)?;
    Some(first_next.pred_opt()?.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(date: &str, cost: f64) -> DailyUsageRow {
        DailyUsageRow {
            metric_date: date.to_string(),
            provider: "test".into(),
            model: "test".into(),
            input_tokens: 0,
            cached_tokens: 0,
            output_tokens: 0,
            cost,
        }
    }

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Days-in-month parity check across calendar quirks. This is the
    /// foundation of every other forecast assertion — if days_in_month
    /// is wrong, every test below is meaningless.
    #[test]
    fn days_in_month_handles_jan_feb_dec_leap_years() {
        assert_eq!(days_in_month(ymd(2026, 1, 15)), Some(31));
        assert_eq!(days_in_month(ymd(2026, 2, 1)), Some(28)); // not a leap year
        assert_eq!(days_in_month(ymd(2024, 2, 1)), Some(29)); // leap year
        assert_eq!(days_in_month(ymd(2026, 12, 31)), Some(31));
        assert_eq!(days_in_month(ymd(2026, 4, 7)), Some(30));
    }

    /// Empty input on day-5: Swift returns a zero forecast (not nil),
    /// is_reliable=false. Verify Rust matches.
    #[test]
    fn forecast_with_zero_data_returns_unreliable_zero() {
        let f = forecast_from_daily(&[], ymd(2026, 5, 5)).unwrap();
        assert_eq!(f.actual_to_date, 0.0);
        assert_eq!(f.predicted_month_total, 0.0);
        assert_eq!(f.lower_bound, 0.0);
        assert_eq!(f.upper_bound, 0.0);
        assert!(!f.is_reliable);
        assert_eq!(f.current_day_of_month, 5);
        assert_eq!(f.days_in_month, 31);
        assert_eq!(f.data_point_count, 5);
    }

    /// Three uniform days at $1/day, current day = 3. Simple-projection
    /// path dominates (regression_weight = 3/14 ≈ 0.21). Predicted
    /// month total ≈ $1 × 31 = $31 (May has 31 days). is_reliable=true.
    #[test]
    fn forecast_with_three_uniform_days_predicts_avg_times_days_in_month() {
        let daily = vec![
            row("2026-05-01", 1.0),
            row("2026-05-02", 1.0),
            row("2026-05-03", 1.0),
        ];
        let f = forecast_from_daily(&daily, ymd(2026, 5, 3)).unwrap();
        assert_eq!(f.actual_to_date, 3.0);
        assert_eq!(f.data_point_count, 3);
        assert!(f.is_reliable);
        // Uniform input → regression slope ≈ 0 → blended ≈ simple
        // projection ≈ $1 × 31 = $31. Tolerance ±0.01.
        assert!(
            (f.predicted_month_total - 31.0).abs() < 0.01,
            "expected ~31.0, got {}",
            f.predicted_month_total
        );
    }

    /// The iter21 last-day-of-month regression. May 31 → no
    /// extrapolation, predicted = actual_to_date.
    #[test]
    fn forecast_on_last_day_of_month_returns_actual_no_extrapolation() {
        let daily: Vec<DailyUsageRow> = (1..=31)
            .map(|d| row(&format!("2026-05-{:02}", d), 2.0))
            .collect();
        let f = forecast_from_daily(&daily, ymd(2026, 5, 31)).unwrap();
        assert_eq!(f.actual_to_date, 62.0);
        assert_eq!(f.data_point_count, 31);
        // No remaining days → projected == actual_to_date.
        // Blended = projected * 0.8 + simple_projection * 0.2,
        //         = 62 * 0.8 + (62/31 * 31) * 0.2
        //         = 49.6 + 12.4 = 62.0
        // Predicted = max(blended, actual) = max(62, 62) = 62.
        assert!(
            (f.predicted_month_total - 62.0).abs() < 0.01,
            "expected exactly 62.0 on last day, got {}",
            f.predicted_month_total
        );
    }

    /// Lower-bound clamp: if blended - margin would dip below the
    /// already-spent total, clamp at actual_to_date. Swift's
    /// `max(blended - margin, actual)` rule.
    #[test]
    fn forecast_clamps_lower_bound_at_actual_to_date() {
        // High variance synthetic data: alternating $0 and $10 days.
        // High stddev, but actual_to_date stays at the cumulative
        // total. Lower bound should not undershoot it.
        let daily = vec![
            row("2026-05-01", 0.0),
            row("2026-05-02", 10.0),
            row("2026-05-03", 0.0),
            row("2026-05-04", 10.0),
            row("2026-05-05", 0.0),
        ];
        let f = forecast_from_daily(&daily, ymd(2026, 5, 5)).unwrap();
        assert_eq!(f.actual_to_date, 20.0);
        assert!(
            f.lower_bound >= f.actual_to_date - 0.0001,
            "lower_bound ({}) must not undershoot actual_to_date ({})",
            f.lower_bound,
            f.actual_to_date
        );
    }

    /// n=2 still computes a forecast, but `is_reliable=false`. The
    /// regression returns a real slope (2 points define a line), but
    /// "reliable" requires >= 3 days of data per Swift's rule.
    #[test]
    fn forecast_with_two_points_marks_unreliable() {
        let daily = vec![row("2026-05-01", 5.0), row("2026-05-02", 7.0)];
        let f = forecast_from_daily(&daily, ymd(2026, 5, 2)).unwrap();
        assert_eq!(f.actual_to_date, 12.0);
        assert_eq!(f.data_point_count, 2);
        assert!(!f.is_reliable);
        // Forecast is still computed (not zero) — Swift parity.
        assert!(f.predicted_month_total > f.actual_to_date);
    }

    /// Growing trend: cost increases each day. Regression slope > 0,
    /// blended projection should exceed simple-average projection.
    #[test]
    fn forecast_with_growing_trend_extrapolates_via_regression() {
        // Days 1-7 with linearly growing cost: $1, $2, $3, ..., $7.
        let daily: Vec<DailyUsageRow> = (1..=7)
            .map(|d| row(&format!("2026-05-{:02}", d), d as f64))
            .collect();
        let f = forecast_from_daily(&daily, ymd(2026, 5, 7)).unwrap();
        // actual_to_date = 1+2+3+4+5+6+7 = 28.
        assert_eq!(f.actual_to_date, 28.0);
        // Simple-avg projection: 28/7 * 31 = 124.0.
        // Regression slope ≈ 1.0, intercept ≈ 0 → projects continued
        // linear growth → larger total. Blended should exceed simple.
        let simple_avg = 28.0 / 7.0 * 31.0;
        assert!(
            f.predicted_month_total > simple_avg,
            "growing trend should project higher than flat-average ({} vs {})",
            f.predicted_month_total,
            simple_avg
        );
        assert!(f.is_reliable);
    }

    /// Aggregation parity: multiple rows for the same date (different
    /// providers/models) sum into one per-day cost.
    #[test]
    fn aggregate_sums_costs_across_provider_model_rows_for_same_date() {
        let daily = vec![
            row("2026-05-01", 1.0),
            DailyUsageRow {
                metric_date: "2026-05-01".into(),
                provider: "claude".into(),
                model: "haiku".into(),
                input_tokens: 0,
                cached_tokens: 0,
                output_tokens: 0,
                cost: 0.5,
            },
            DailyUsageRow {
                metric_date: "2026-05-01".into(),
                provider: "openrouter".into(),
                model: "gpt-4".into(),
                input_tokens: 0,
                cached_tokens: 0,
                output_tokens: 0,
                cost: 2.0,
            },
        ];
        let agg = aggregate_cost_by_date(&daily);
        assert_eq!(agg.get("2026-05-01"), Some(&3.5));
    }
}
