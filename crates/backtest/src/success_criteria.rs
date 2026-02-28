use crate::oracle_replay::BacktestResult;

/// Result of a go/no-go evaluation.
#[derive(Debug, Clone)]
pub struct Assessment {
    pub passed: bool,
    pub expectancy_passed: bool,
    pub profit_factor_passed: bool,
    pub win_rate_passed: bool,
    pub drawdown_passed: bool,
    pub trade_count_passed: bool,
    pub oos_pnl_passed: bool,

    pub actual_expectancy: f32,
    pub actual_profit_factor: f32,
    pub actual_win_rate: f32,
    pub actual_max_drawdown: f32,
    pub actual_trades_per_day: f32,

    pub decision: String,
}

impl Default for Assessment {
    fn default() -> Self {
        Self {
            passed: false,
            expectancy_passed: false,
            profit_factor_passed: false,
            win_rate_passed: false,
            drawdown_passed: false,
            trade_count_passed: false,
            oos_pnl_passed: false,
            actual_expectancy: 0.0,
            actual_profit_factor: 0.0,
            actual_win_rate: 0.0,
            actual_max_drawdown: 0.0,
            actual_trades_per_day: 0.0,
            decision: "NO-GO".to_string(),
        }
    }
}

/// Threshold-based go/no-go framework.
#[derive(Debug, Clone)]
pub struct SuccessCriteria {
    pub min_expectancy: f32,
    pub min_profit_factor: f32,
    pub min_win_rate: f32,
    pub max_drawdown_multiple: f32,
    pub min_trades_per_day: f32,
    pub max_safety_cap_fraction: f32,
}

impl Default for SuccessCriteria {
    fn default() -> Self {
        Self {
            min_expectancy: 0.50,
            min_profit_factor: 1.3,
            min_win_rate: 0.45,
            max_drawdown_multiple: 50.0,
            min_trades_per_day: 10.0,
            max_safety_cap_fraction: 0.01,
        }
    }
}

impl SuccessCriteria {
    pub fn evaluate(&self, result: &BacktestResult) -> Assessment {
        let mut a = Assessment::default();
        a.actual_expectancy = result.expectancy;
        a.actual_profit_factor = result.profit_factor;
        a.actual_win_rate = result.win_rate;
        a.actual_max_drawdown = result.max_drawdown;
        a.actual_trades_per_day = result.trades_per_day;

        a.expectancy_passed = result.expectancy > self.min_expectancy;
        a.profit_factor_passed = result.profit_factor > self.min_profit_factor;
        a.win_rate_passed = result.win_rate > self.min_win_rate;

        let max_allowed_dd = self.max_drawdown_multiple * result.expectancy;
        a.drawdown_passed = result.max_drawdown < max_allowed_dd;

        a.trade_count_passed = result.trades_per_day > self.min_trades_per_day;

        a.passed = a.expectancy_passed
            && a.profit_factor_passed
            && a.win_rate_passed
            && a.drawdown_passed
            && a.trade_count_passed;
        a.decision = if a.passed {
            "GO".to_string()
        } else {
            "NO-GO".to_string()
        };

        a
    }

    pub fn evaluate_with_oos(
        &self,
        is_result: &BacktestResult,
        oos_result: &BacktestResult,
    ) -> Assessment {
        let mut a = self.evaluate(is_result);
        a.oos_pnl_passed = oos_result.net_pnl > 0.0;
        a.passed = a.passed && a.oos_pnl_passed;
        a.decision = if a.passed {
            "GO".to_string()
        } else {
            "NO-GO".to_string()
        };
        a
    }

    pub fn safety_cap_ok(&self, result: &BacktestResult) -> bool {
        result.safety_cap_fraction < self.max_safety_cap_fraction
    }
}

/// Oracle diagnosis output.
#[derive(Debug, Clone)]
pub struct OracleDiagnosis {
    pub recommendations: Vec<String>,
    pub continue_to_phase4: bool,
}

pub fn diagnose(result: &BacktestResult) -> OracleDiagnosis {
    let mut diag = OracleDiagnosis {
        recommendations: Vec::new(),
        continue_to_phase4: true,
    };

    if result.gross_pnl > 0.0 && result.net_pnl < 0.0 {
        diag.recommendations.push(
            "Costs too high for scale — try larger targets (20, 40 ticks)".to_string(),
        );
    }

    if result.safety_cap_fraction > 0.01 {
        diag.recommendations.push(
            "MES microstructure too noisy — filter: only label when spread < 2 ticks".to_string(),
        );
    }

    if result.expectancy < -0.5 && result.win_rate < 0.30 {
        diag.recommendations.push(
            "Oracle threshold logic too naive — proceed to feature discovery on returns"
                .to_string(),
        );
    }

    if result.net_pnl < 0.0 && diag.recommendations.is_empty() {
        diag.recommendations.push(
            "Negative net PnL — feature discovery may reveal better labels".to_string(),
        );
    }

    diag.recommendations.push(
        "Continue to Phase 4 — feature discovery may improve results".to_string(),
    );

    diag
}
