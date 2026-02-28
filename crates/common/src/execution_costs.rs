/// Spread model for execution cost computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadModel {
    Fixed,
    Empirical,
}

/// MES futures execution cost model.
///
/// Models commission, spread, and slippage costs for round-trip trades.
/// Default values calibrated for MES (Micro E-mini S&P 500) futures.
#[derive(Debug, Clone)]
pub struct ExecutionCosts {
    /// Commission per side in USD (default: $0.62).
    pub commission_per_side: f32,
    /// Spread model: Fixed or Empirical.
    pub spread_model: SpreadModel,
    /// Fixed spread in ticks (used when spread_model == Fixed).
    pub fixed_spread_ticks: i32,
    /// Slippage in ticks.
    pub slippage_ticks: i32,
    /// Contract multiplier (default: $5.00 for MES).
    pub contract_multiplier: f32,
    /// Tick size (default: $0.25 for MES).
    pub tick_size: f32,
    /// Tick value = contract_multiplier * tick_size (default: $1.25).
    pub tick_value: f32,
}

impl Default for ExecutionCosts {
    fn default() -> Self {
        Self {
            commission_per_side: 0.62,
            spread_model: SpreadModel::Fixed,
            fixed_spread_ticks: 1,
            slippage_ticks: 0,
            contract_multiplier: 5.0,
            tick_size: 0.25,
            tick_value: 1.25, // 5.0 * 0.25
        }
    }
}

impl ExecutionCosts {
    /// Per-side cost: commission + half-spread cost + slippage cost.
    ///
    /// In Fixed mode, `actual_spread_ticks` is ignored; `fixed_spread_ticks` is used.
    /// In Empirical mode, `actual_spread_ticks` is used instead.
    pub fn per_side_cost(&self, actual_spread_ticks: f32) -> f32 {
        let spread_ticks_used = match self.spread_model {
            SpreadModel::Fixed => self.fixed_spread_ticks as f32,
            SpreadModel::Empirical => actual_spread_ticks,
        };
        let half_spread_cost = (spread_ticks_used / 2.0) * self.tick_value;
        let slippage_cost = self.slippage_ticks as f32 * self.tick_value;
        self.commission_per_side + half_spread_cost + slippage_cost
    }

    /// Round-trip cost: entry per-side cost + exit per-side cost.
    pub fn round_trip_cost(&self, entry_spread_ticks: f32, exit_spread_ticks: f32) -> f32 {
        self.per_side_cost(entry_spread_ticks) + self.per_side_cost(exit_spread_ticks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_per_side_cost() {
        let costs = ExecutionCosts::default();
        // Fixed mode: commission(0.62) + half_spread(0.5 * 1.25 = 0.625) + slippage(0) = 1.245
        let psc = costs.per_side_cost(0.0);
        assert!((psc - 1.245).abs() < 1e-6);
    }

    #[test]
    fn test_default_round_trip_cost() {
        let costs = ExecutionCosts::default();
        // Round trip = 2 * per_side = 2.49
        let rtc = costs.round_trip_cost(0.0, 0.0);
        assert!((rtc - 2.49).abs() < 1e-6);
    }

    #[test]
    fn test_empirical_spread() {
        let costs = ExecutionCosts {
            spread_model: SpreadModel::Empirical,
            ..Default::default()
        };
        // 2 tick spread: commission(0.62) + half_spread(1.0 * 1.25 = 1.25) + slippage(0) = 1.87
        let psc = costs.per_side_cost(2.0);
        assert!((psc - 1.87).abs() < 1e-6);
    }

    #[test]
    fn test_with_slippage() {
        let costs = ExecutionCosts {
            slippage_ticks: 1,
            ..Default::default()
        };
        // Fixed: commission(0.62) + half_spread(0.625) + slippage(1 * 1.25 = 1.25) = 2.495
        let psc = costs.per_side_cost(0.0);
        assert!((psc - 2.495).abs() < 1e-6);
    }
}
