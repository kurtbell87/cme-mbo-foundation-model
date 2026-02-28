/// MBO event from Databento wire format (simplified for event buffer).

#[derive(Debug, Clone, Default)]
pub struct MBOEvent {
    /// Action: Add=0, Cancel=1, Modify=2, Trade=3.
    pub action: i32,
    /// Price as float.
    pub price: f32,
    /// Size in contracts.
    pub size: u32,
    /// Side: Bid=0, Ask=1.
    pub side: i32,
    /// Event timestamp in nanoseconds.
    pub ts_event: u64,
}

/// Buffer for a day's MBO events, supporting range queries.
#[derive(Debug, Clone, Default)]
pub struct DayEventBuffer {
    events: Vec<MBOEvent>,
}

impl DayEventBuffer {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Get a slice of events in `[begin, end)`.
    pub fn get_events(&self, begin: u32, end: u32) -> &[MBOEvent] {
        let begin = begin as usize;
        let end = end as usize;
        if begin >= end || begin >= self.events.len() {
            return &[];
        }
        let actual_end = end.min(self.events.len());
        &self.events[begin..actual_end]
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.events.shrink_to_fit();
    }

    pub fn push(&mut self, event: MBOEvent) {
        self.events.push(event);
    }
}
