/// Exit reason codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitReason {
    Target = 0,
    Stop = 1,
    TakeProfit = 2,
    Expiry = 3,
    SessionEnd = 4,
    SafetyCap = 5,
}

impl ExitReason {
    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => ExitReason::Target,
            1 => ExitReason::Stop,
            2 => ExitReason::TakeProfit,
            3 => ExitReason::Expiry,
            4 => ExitReason::SessionEnd,
            5 => ExitReason::SafetyCap,
            _ => ExitReason::SessionEnd,
        }
    }
}

/// Record of a single trade.
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub entry_ts: u64,
    pub exit_ts: u64,
    pub entry_price: f32,
    pub exit_price: f32,
    pub direction: i32, // +1 = LONG, -1 = SHORT
    pub gross_pnl: f32,
    pub net_pnl: f32,
    pub entry_bar_idx: usize,
    pub exit_bar_idx: usize,
    pub bars_held: i32,
    pub duration_s: f32,
    pub exit_reason: ExitReason,
}

impl Default for TradeRecord {
    fn default() -> Self {
        Self {
            entry_ts: 0,
            exit_ts: 0,
            entry_price: 0.0,
            exit_price: 0.0,
            direction: 0,
            gross_pnl: 0.0,
            net_pnl: 0.0,
            entry_bar_idx: 0,
            exit_bar_idx: 0,
            bars_held: 0,
            duration_s: 0.0,
            exit_reason: ExitReason::SessionEnd,
        }
    }
}
