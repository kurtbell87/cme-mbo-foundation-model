pub mod trade_record;
pub mod oracle_replay;
pub mod multi_day_runner;
pub mod rollover;
pub mod regime_stratification;
pub mod success_criteria;

pub use trade_record::{ExitReason, TradeRecord};
pub use oracle_replay::{BacktestResult, OracleConfig, OracleReplay};
pub use multi_day_runner::{BacktestConfig, DayResult, DaySchedule, MultiDayRunner, SplitResults};
pub use rollover::{ContractSpec, RolloverCalendar};
pub use regime_stratification::{RegimeResult, RegimeStratifier, Session, Trend};
pub use success_criteria::{Assessment, OracleDiagnosis, SuccessCriteria};
