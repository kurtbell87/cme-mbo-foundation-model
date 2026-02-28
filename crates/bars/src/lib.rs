mod builder_base;
mod dollar_bar;
mod tick_bar;
mod time_bar;
mod volume_bar;

pub use builder_base::TradeInfo;
pub use dollar_bar::DollarBarBuilder;
pub use tick_bar::TickBarBuilder;
pub use time_bar::TimeBarBuilder;
pub use volume_bar::VolumeBarBuilder;

use common::bar::Bar;
use common::book::BookSnapshot;

/// Abstract interface for bar construction.
///
/// Implementations receive 100ms BookSnapshots and emit completed Bars.
pub trait BarBuilder {
    /// Process a snapshot. Returns a completed Bar if a bar boundary was crossed.
    fn on_snapshot(&mut self, snap: &BookSnapshot) -> Option<Bar>;

    /// Flush any partial bar at end of session.
    fn flush(&mut self) -> Option<Bar>;
}

/// Factory for creating bar builders by type name.
pub fn create_bar_builder(bar_type: &str, threshold: f64) -> Option<Box<dyn BarBuilder>> {
    match bar_type {
        "volume" => Some(Box::new(VolumeBarBuilder::new(threshold as u32))),
        "tick" => Some(Box::new(TickBarBuilder::new(threshold as u32))),
        "dollar" => Some(Box::new(DollarBarBuilder::new(threshold, 5.0))),
        "time" => Some(Box::new(TimeBarBuilder::new(threshold as u64))),
        _ => None,
    }
}
