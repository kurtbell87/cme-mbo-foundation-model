//! MBO event tokenizer for transformer/SSM grammar learning.
//!
//! Converts raw MBO (Market By Order) events into discrete token sequences
//! suitable for autoregressive language model training. Each order book event
//! becomes a short sub-token sequence: `[ACTION] [SIDE] [PRICE_REL] [SIZE]`,
//! with `[COMMIT]` tokens marking F_LAST (atomic batch) boundaries.
//!
//! **Price encoding:** relative to current mid-price in integer ticks, rounded.
//! Range ±50 ticks, with `FAR_NEG`/`FAR_POS` overflow tokens and `NO_REF`
//! when the book is one-sided.
//!
//! **Size encoding:** log₂-quantized into 9 buckets (0, 1, 2-3, 4-7, ..., 128+).
//!
//! **Design rationale:** Multi-token encoding (vs single-token-per-event) preserves
//! compositional structure. `[ADD] [BID]` shares learned representations with
//! `[ADD] [ASK]`, just as "running quickly" shares structure with "running slowly"
//! in natural language. This is critical for the Physics-of-Language-Models
//! grammar-learning hypothesis.

use book_builder::BookBuilder;

// ═══════════════════════════════════════════════════════════════
// Vocabulary constants
// ═══════════════════════════════════════════════════════════════
//
// Layout (126 tokens total):
//   [0..3]     Special: PAD, BOS, EOS, COMMIT
//   [4..9]     Actions: ADD, CANCEL, MODIFY, TRADE, FILL, CLEAR
//   [10..12]   Sides:   BID, ASK, NONE
//   [13..116]  Price:   FAR_NEG, {-50..+50}, FAR_POS, NO_REF  (104 tokens)
//   [117..125] Size:    0, 1, 2-3, 4-7, 8-15, 16-31, 32-63, 64-127, 128+

pub const VOCAB_SIZE: usize = 126;

// ── Special tokens ──────────────────────────────────────────
pub const PAD: u16 = 0;
pub const BOS: u16 = 1;
pub const EOS: u16 = 2;
pub const COMMIT: u16 = 3;

// ── Action tokens ───────────────────────────────────────────
pub const ACT_ADD: u16 = 4;
pub const ACT_CANCEL: u16 = 5;
pub const ACT_MODIFY: u16 = 6;
pub const ACT_TRADE: u16 = 7;
pub const ACT_FILL: u16 = 8;
pub const ACT_CLEAR: u16 = 9;

// ── Side tokens ─────────────────────────────────────────────
pub const SIDE_BID: u16 = 10;
pub const SIDE_ASK: u16 = 11;
pub const SIDE_NONE: u16 = 12;

// ── Price tokens ────────────────────────────────────────────
//
// Token 13        = FAR_NEG  (delta < -50)
// Token 14..=114  = delta -50..=+50  (token = 64 + delta)
// Token 115       = FAR_POS  (delta > +50)
// Token 116       = NO_REF   (no valid mid-price)
pub const PRICE_FAR_NEG: u16 = 13;
pub const PRICE_TICK_BASE: u16 = 14; // token for delta = -50
pub const PRICE_ZERO: u16 = 64; // token for delta = 0
pub const PRICE_FAR_POS: u16 = 115;
pub const PRICE_NO_REF: u16 = 116;
pub const PRICE_RANGE: i32 = 50;

// ── Size tokens ─────────────────────────────────────────────
pub const SZ_0: u16 = 117;
pub const SZ_1: u16 = 118;
pub const SZ_2_3: u16 = 119;
pub const SZ_4_7: u16 = 120;
pub const SZ_8_15: u16 = 121;
pub const SZ_16_31: u16 = 122;
pub const SZ_32_63: u16 = 123;
pub const SZ_64_127: u16 = 124;
pub const SZ_128_PLUS: u16 = 125;

// ═══════════════════════════════════════════════════════════════
// Encoding
// ═══════════════════════════════════════════════════════════════

/// Encode a Databento action char to a token.
pub fn encode_action(action: char) -> Option<u16> {
    match action {
        'A' => Some(ACT_ADD),
        'C' => Some(ACT_CANCEL),
        'M' => Some(ACT_MODIFY),
        'T' => Some(ACT_TRADE),
        'F' => Some(ACT_FILL),
        'R' => Some(ACT_CLEAR),
        _ => None,
    }
}

/// Encode a Databento side char to a token.
pub fn encode_side(side: char) -> u16 {
    match side {
        'B' => SIDE_BID,
        'A' => SIDE_ASK,
        _ => SIDE_NONE,
    }
}

/// Encode a price delta (integer ticks from mid) to a token.
///
/// Ticks outside `[-50, +50]` are clamped to `FAR_NEG`/`FAR_POS`.
pub fn encode_price_ticks(ticks: i32) -> u16 {
    if ticks < -PRICE_RANGE {
        PRICE_FAR_NEG
    } else if ticks > PRICE_RANGE {
        PRICE_FAR_POS
    } else {
        (PRICE_ZERO as i32 + ticks) as u16
    }
}

/// Compute the price delta in integer ticks from fixed-point prices.
///
/// `event_price` and `mid` are i64 fixed-point (×1e9).
/// `tick_size_fixed` is the tick size in the same units.
/// Returns the rounded integer tick delta.
pub fn compute_tick_delta(event_price: i64, mid: i64, tick_size_fixed: i64) -> i32 {
    let delta = event_price - mid;
    (delta as f64 / tick_size_fixed as f64).round() as i32
}

/// Encode a contract size to a log₂-quantized token.
pub fn encode_size(size: u32) -> u16 {
    match size {
        0 => SZ_0,
        1 => SZ_1,
        2..=3 => SZ_2_3,
        4..=7 => SZ_4_7,
        8..=15 => SZ_8_15,
        16..=31 => SZ_16_31,
        32..=63 => SZ_32_63,
        64..=127 => SZ_64_127,
        _ => SZ_128_PLUS,
    }
}

// ═══════════════════════════════════════════════════════════════
// Decoding
// ═══════════════════════════════════════════════════════════════

/// Decoded price delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceDelta {
    /// Exact tick offset from mid.
    Ticks(i32),
    /// More than 50 ticks below mid.
    FarNeg,
    /// More than 50 ticks above mid.
    FarPos,
    /// No valid mid-price was available.
    NoRef,
}

/// Decode an action token back to its char.
pub fn decode_action(token: u16) -> Option<char> {
    match token {
        ACT_ADD => Some('A'),
        ACT_CANCEL => Some('C'),
        ACT_MODIFY => Some('M'),
        ACT_TRADE => Some('T'),
        ACT_FILL => Some('F'),
        ACT_CLEAR => Some('R'),
        _ => None,
    }
}

/// Decode a side token back to its char.
pub fn decode_side(token: u16) -> Option<char> {
    match token {
        SIDE_BID => Some('B'),
        SIDE_ASK => Some('A'),
        SIDE_NONE => Some(' '),
        _ => None,
    }
}

/// Decode a price token to a `PriceDelta`.
pub fn decode_price(token: u16) -> Option<PriceDelta> {
    match token {
        PRICE_FAR_NEG => Some(PriceDelta::FarNeg),
        PRICE_FAR_POS => Some(PriceDelta::FarPos),
        PRICE_NO_REF => Some(PriceDelta::NoRef),
        PRICE_TICK_BASE..=114 => Some(PriceDelta::Ticks(token as i32 - PRICE_ZERO as i32)),
        _ => None,
    }
}

/// Decode a size token to its representative value (lower bound of bucket).
pub fn decode_size(token: u16) -> Option<u32> {
    match token {
        SZ_0 => Some(0),
        SZ_1 => Some(1),
        SZ_2_3 => Some(2),
        SZ_4_7 => Some(4),
        SZ_8_15 => Some(8),
        SZ_16_31 => Some(16),
        SZ_32_63 => Some(32),
        SZ_64_127 => Some(64),
        SZ_128_PLUS => Some(128),
        _ => None,
    }
}

/// Human-readable name for any token.
pub fn token_name(token: u16) -> String {
    match token {
        PAD => "PAD".into(),
        BOS => "BOS".into(),
        EOS => "EOS".into(),
        COMMIT => "COMMIT".into(),
        ACT_ADD => "ADD".into(),
        ACT_CANCEL => "CANCEL".into(),
        ACT_MODIFY => "MODIFY".into(),
        ACT_TRADE => "TRADE".into(),
        ACT_FILL => "FILL".into(),
        ACT_CLEAR => "CLEAR".into(),
        SIDE_BID => "BID".into(),
        SIDE_ASK => "ASK".into(),
        SIDE_NONE => "SIDE:NONE".into(),
        PRICE_FAR_NEG => "P:FAR-".into(),
        PRICE_FAR_POS => "P:FAR+".into(),
        PRICE_NO_REF => "P:NOREF".into(),
        PRICE_TICK_BASE..=114 => {
            let d = token as i32 - PRICE_ZERO as i32;
            format!("P:{d:+}")
        }
        SZ_0 => "SZ:0".into(),
        SZ_1 => "SZ:1".into(),
        SZ_2_3 => "SZ:2-3".into(),
        SZ_4_7 => "SZ:4-7".into(),
        SZ_8_15 => "SZ:8-15".into(),
        SZ_16_31 => "SZ:16-31".into(),
        SZ_32_63 => "SZ:32-63".into(),
        SZ_64_127 => "SZ:64-127".into(),
        SZ_128_PLUS => "SZ:128+".into(),
        _ => format!("UNK:{token}"),
    }
}

/// Format a token sequence as a human-readable string.
pub fn display_tokens(tokens: &[u16]) -> String {
    tokens
        .iter()
        .map(|t| token_name(*t))
        .collect::<Vec<_>>()
        .join(" ")
}

// ═══════════════════════════════════════════════════════════════
// Tokenizer
// ═══════════════════════════════════════════════════════════════

const F_LAST: u8 = 0x80;

/// Accumulated statistics from tokenization.
#[derive(Debug, Clone)]
pub struct TokenizerStats {
    pub events_processed: u64,
    pub events_skipped_instrument: u64,
    pub tokens_emitted: u64,
    pub commits: u64,
    /// Per-action counts: [ADD, CANCEL, MODIFY, TRADE, FILL, CLEAR].
    pub action_counts: [u64; 6],
    pub price_far_neg: u64,
    pub price_far_pos: u64,
    pub price_no_ref: u64,
    /// Histogram of price tokens (indices 0..=102 → FAR_NEG, -50..+50, FAR_POS).
    pub price_histogram: [u64; 103],
    /// Per-size-bucket counts (9 buckets).
    pub size_histogram: [u64; 9],
}

impl Default for TokenizerStats {
    fn default() -> Self {
        Self {
            events_processed: 0,
            events_skipped_instrument: 0,
            tokens_emitted: 0,
            commits: 0,
            action_counts: [0; 6],
            price_far_neg: 0,
            price_far_pos: 0,
            price_no_ref: 0,
            price_histogram: [0; 103],
            size_histogram: [0; 9],
        }
    }
}

impl TokenizerStats {
    pub fn price_in_range_pct(&self) -> f64 {
        let in_range = self.events_processed - self.price_far_neg - self.price_far_pos - self.price_no_ref;
        if self.events_processed == 0 {
            0.0
        } else {
            in_range as f64 / self.events_processed as f64 * 100.0
        }
    }
}

impl std::fmt::Display for TokenizerStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Tokenizer Stats:")?;
        writeln!(f, "  events processed:    {}", self.events_processed)?;
        writeln!(f, "  events skipped (id): {}", self.events_skipped_instrument)?;
        writeln!(f, "  tokens emitted:      {}", self.tokens_emitted)?;
        writeln!(f, "  commits (F_LAST):    {}", self.commits)?;
        writeln!(f, "  tokens/event:        {:.2}",
            if self.events_processed > 0 {
                self.tokens_emitted as f64 / self.events_processed as f64
            } else { 0.0 })?;
        writeln!(f, "  Actions: ADD={} CANCEL={} MODIFY={} TRADE={} FILL={} CLEAR={}",
            self.action_counts[0], self.action_counts[1], self.action_counts[2],
            self.action_counts[3], self.action_counts[4], self.action_counts[5])?;
        writeln!(f, "  Price: in-range={:.1}% far-={} far+={} no-ref={}",
            self.price_in_range_pct(),
            self.price_far_neg, self.price_far_pos, self.price_no_ref)?;
        write!(f, "  Size: ")?;
        let labels = ["0", "1", "2-3", "4-7", "8-15", "16-31", "32-63", "64-127", "128+"];
        for (i, label) in labels.iter().enumerate() {
            if i > 0 { write!(f, " ")?; }
            write!(f, "[{}]={}", label, self.size_histogram[i])?;
        }
        writeln!(f)
    }
}

/// Stateful tokenizer that converts raw MBO events to token sequences.
///
/// Maintains a `BookBuilder` internally to track the current BBO (needed for
/// relative price encoding). Events are encoded using the **pre-event** mid-price
/// — the price reference is the book state *before* the event is applied.
pub struct MboTokenizer {
    book: BookBuilder,
    instrument_id: u32,
    tick_size_fixed: i64,
    stats: TokenizerStats,
}

impl MboTokenizer {
    /// Create a new tokenizer for a specific instrument.
    ///
    /// `tick_size` is in price units (e.g., 0.25 for MES).
    pub fn new(instrument_id: u32, tick_size: f64) -> Self {
        assert!(tick_size > 0.0, "tick_size must be positive");
        let tick_size_fixed = (tick_size * 1e9).round() as i64;
        Self {
            book: BookBuilder::new(instrument_id),
            instrument_id,
            tick_size_fixed,
            stats: TokenizerStats::default(),
        }
    }

    /// Feed a raw MBO event and append resulting tokens to `out`.
    ///
    /// Returns the number of tokens emitted for this event (0 if skipped).
    ///
    /// **Token output per event type:**
    /// - Normal (A/C/M/T/F): `[ACTION] [SIDE] [PRICE] [SIZE]` (4 tokens)
    /// - Clear (R): `[CLEAR]` (1 token)
    /// - F_LAST flag adds: `[COMMIT]` (+1 token)
    pub fn feed_event(
        &mut self,
        ts_event: u64,
        order_id: u64,
        instrument_id: u32,
        action: char,
        side: char,
        price: i64,
        size: u32,
        flags: u8,
        out: &mut Vec<u16>,
    ) -> usize {
        // Skip events for other instruments
        if instrument_id != self.instrument_id {
            self.stats.events_skipped_instrument += 1;
            return 0;
        }

        self.stats.events_processed += 1;
        let start_len = out.len();

        // 1. Capture pre-event mid for relative price encoding
        let mid_fixed = self.current_mid_fixed();

        // 2. Encode the event
        match action {
            'R' => {
                out.push(ACT_CLEAR);
                self.stats.action_counts[5] += 1;
            }
            _ => {
                // Action
                let act_token = match encode_action(action) {
                    Some(t) => t,
                    None => {
                        // Unknown action — feed to book and skip
                        self.book.process_event(
                            ts_event, order_id, instrument_id, action, side, price, size, flags,
                        );
                        return 0;
                    }
                };
                let act_idx = match action {
                    'A' => 0,
                    'C' => 1,
                    'M' => 2,
                    'T' => 3,
                    'F' => 4,
                    _ => unreachable!(),
                };
                self.stats.action_counts[act_idx] += 1;

                // Side
                let side_token = encode_side(side);

                // Price (relative to pre-event mid)
                let price_token = match mid_fixed {
                    Some(mid) => {
                        let ticks = compute_tick_delta(price, mid, self.tick_size_fixed);
                        let token = encode_price_ticks(ticks);
                        // Histogram
                        if ticks < -PRICE_RANGE {
                            self.stats.price_far_neg += 1;
                            self.stats.price_histogram[0] += 1;
                        } else if ticks > PRICE_RANGE {
                            self.stats.price_far_pos += 1;
                            self.stats.price_histogram[102] += 1;
                        } else {
                            self.stats.price_histogram[(ticks + PRICE_RANGE) as usize + 1] += 1;
                        }
                        token
                    }
                    None => {
                        self.stats.price_no_ref += 1;
                        PRICE_NO_REF
                    }
                };

                // Size
                let size_token = encode_size(size);
                let size_bucket = (size_token - SZ_0) as usize;
                self.stats.size_histogram[size_bucket] += 1;

                out.push(act_token);
                out.push(side_token);
                out.push(price_token);
                out.push(size_token);
            }
        }

        // 3. Feed event to BookBuilder (updates book state for NEXT event)
        self.book.process_event(
            ts_event, order_id, instrument_id, action, side, price, size, flags,
        );

        // 4. Emit COMMIT on F_LAST boundary
        if flags & F_LAST != 0 {
            out.push(COMMIT);
            self.stats.commits += 1;
        }

        let emitted = out.len() - start_len;
        self.stats.tokens_emitted += emitted as u64;
        emitted
    }

    /// Read-only access to accumulated statistics.
    pub fn stats(&self) -> &TokenizerStats {
        &self.stats
    }

    /// Current mid-price in fixed-point (i64 × 1e9), or `None` if one-sided.
    fn current_mid_fixed(&self) -> Option<i64> {
        match (self.book.best_bid_price(), self.book.best_ask_price()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2),
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── Vocabulary invariants ───────────────────────────────

    #[test]
    fn vocab_size_is_consistent() {
        // Highest token is SZ_128_PLUS = 125, so VOCAB_SIZE = 126.
        assert_eq!(SZ_128_PLUS, (VOCAB_SIZE - 1) as u16);
    }

    #[test]
    fn price_token_range_is_contiguous() {
        // FAR_NEG = 13, then 101 tick values (14..=114), FAR_POS = 115, NO_REF = 116
        assert_eq!(PRICE_FAR_NEG, 13);
        assert_eq!(PRICE_TICK_BASE, 14); // delta = -50
        assert_eq!(PRICE_ZERO, 64); // delta = 0
        assert_eq!(PRICE_TICK_BASE as i32 + 100, 114); // delta = +50
        assert_eq!(PRICE_FAR_POS, 115);
        assert_eq!(PRICE_NO_REF, 116);
    }

    #[test]
    fn no_token_id_overlaps() {
        // Verify the token ranges don't overlap
        let special = 0..=3u16;
        let actions = 4..=9u16;
        let sides = 10..=12u16;
        let prices = 13..=116u16;
        let sizes = 117..=125u16;

        assert!(!special.contains(actions.start()));
        assert!(!actions.contains(sides.start()));
        assert!(!sides.contains(prices.start()));
        assert!(!prices.contains(sizes.start()));
        assert_eq!(*sizes.end() + 1, VOCAB_SIZE as u16);
    }

    // ── Encode/decode roundtrips ────────────────────────────

    #[test]
    fn action_roundtrip() {
        for ch in ['A', 'C', 'M', 'T', 'F', 'R'] {
            let token = encode_action(ch).unwrap();
            let decoded = decode_action(token).unwrap();
            assert_eq!(ch, decoded, "action roundtrip failed for '{ch}'");
        }
    }

    #[test]
    fn action_unknown_returns_none() {
        assert_eq!(encode_action('X'), None);
        assert_eq!(decode_action(0), None);
        assert_eq!(decode_action(200), None);
    }

    #[test]
    fn side_roundtrip() {
        assert_eq!(decode_side(encode_side('B')).unwrap(), 'B');
        assert_eq!(decode_side(encode_side('A')).unwrap(), 'A');
        assert_eq!(decode_side(encode_side(' ')).unwrap(), ' ');
    }

    #[test]
    fn price_roundtrip_all_ticks() {
        for d in -PRICE_RANGE..=PRICE_RANGE {
            let token = encode_price_ticks(d);
            let decoded = decode_price(token).unwrap();
            assert_eq!(decoded, PriceDelta::Ticks(d), "price roundtrip failed for delta={d}");
        }
    }

    #[test]
    fn price_boundaries() {
        // Exact boundaries
        assert_eq!(encode_price_ticks(-50), PRICE_TICK_BASE); // 14
        assert_eq!(encode_price_ticks(0), PRICE_ZERO); // 64
        assert_eq!(encode_price_ticks(50), 114);

        // Just outside range
        assert_eq!(encode_price_ticks(-51), PRICE_FAR_NEG);
        assert_eq!(encode_price_ticks(51), PRICE_FAR_POS);

        // Way outside range
        assert_eq!(encode_price_ticks(-1000), PRICE_FAR_NEG);
        assert_eq!(encode_price_ticks(1000), PRICE_FAR_POS);
    }

    #[test]
    fn price_far_decode() {
        assert_eq!(decode_price(PRICE_FAR_NEG), Some(PriceDelta::FarNeg));
        assert_eq!(decode_price(PRICE_FAR_POS), Some(PriceDelta::FarPos));
        assert_eq!(decode_price(PRICE_NO_REF), Some(PriceDelta::NoRef));
    }

    #[test]
    fn price_invalid_token() {
        assert_eq!(decode_price(0), None); // PAD
        assert_eq!(decode_price(4), None); // ACT_ADD
        assert_eq!(decode_price(117), None); // SZ_0
    }

    #[test]
    fn size_bucket_boundaries() {
        assert_eq!(encode_size(0), SZ_0);
        assert_eq!(encode_size(1), SZ_1);
        assert_eq!(encode_size(2), SZ_2_3);
        assert_eq!(encode_size(3), SZ_2_3);
        assert_eq!(encode_size(4), SZ_4_7);
        assert_eq!(encode_size(7), SZ_4_7);
        assert_eq!(encode_size(8), SZ_8_15);
        assert_eq!(encode_size(15), SZ_8_15);
        assert_eq!(encode_size(16), SZ_16_31);
        assert_eq!(encode_size(31), SZ_16_31);
        assert_eq!(encode_size(32), SZ_32_63);
        assert_eq!(encode_size(63), SZ_32_63);
        assert_eq!(encode_size(64), SZ_64_127);
        assert_eq!(encode_size(127), SZ_64_127);
        assert_eq!(encode_size(128), SZ_128_PLUS);
        assert_eq!(encode_size(10_000), SZ_128_PLUS);
    }

    #[test]
    fn size_decode() {
        assert_eq!(decode_size(SZ_0), Some(0));
        assert_eq!(decode_size(SZ_1), Some(1));
        assert_eq!(decode_size(SZ_2_3), Some(2));
        assert_eq!(decode_size(SZ_128_PLUS), Some(128));
        assert_eq!(decode_size(0), None); // PAD
    }

    // ── Tick delta computation ──────────────────────────────

    #[test]
    fn tick_delta_at_mid() {
        let tick = 250_000_000i64; // 0.25 in fixed-point
        let mid = 4500_125_000_000i64; // 4500.125 (between bid=4500.00 and ask=4500.25)
        let event_at_mid = mid;
        assert_eq!(compute_tick_delta(event_at_mid, mid, tick), 0);
    }

    #[test]
    fn tick_delta_one_tick_spread() {
        // MES 1-tick spread: bid=4500.00, ask=4500.25, mid=4500.125
        let tick = 250_000_000i64;
        let bid = 4500_000_000_000i64;
        let ask = 4500_250_000_000i64;
        let mid = (bid + ask) / 2; // 4500_125_000_000

        // Bid is 0.5 ticks below mid → rounds to -1 (round-half-away-from-zero)
        assert_eq!(compute_tick_delta(bid, mid, tick), -1);
        // Ask is 0.5 ticks above mid → rounds to +1
        assert_eq!(compute_tick_delta(ask, mid, tick), 1);
    }

    #[test]
    fn tick_delta_two_tick_spread() {
        // bid=4500.00, ask=4500.50, mid=4500.25
        let tick = 250_000_000i64;
        let bid = 4500_000_000_000i64;
        let ask = 4500_500_000_000i64;
        let mid = (bid + ask) / 2; // 4500_250_000_000

        // Bid is exactly 1 tick below mid
        assert_eq!(compute_tick_delta(bid, mid, tick), -1);
        // Ask is exactly 1 tick above mid
        assert_eq!(compute_tick_delta(ask, mid, tick), 1);
    }

    #[test]
    fn tick_delta_deeper_levels() {
        let tick = 250_000_000i64;
        let mid = 4500_125_000_000i64;

        // 5 ticks below mid
        let price = mid - 5 * tick;
        assert_eq!(compute_tick_delta(price, mid, tick), -5);

        // 10 ticks above mid
        let price = mid + 10 * tick;
        assert_eq!(compute_tick_delta(price, mid, tick), 10);
    }

    // ── Token display ───────────────────────────────────────

    #[test]
    fn token_names_are_readable() {
        assert_eq!(token_name(PAD), "PAD");
        assert_eq!(token_name(BOS), "BOS");
        assert_eq!(token_name(ACT_ADD), "ADD");
        assert_eq!(token_name(SIDE_BID), "BID");
        assert_eq!(token_name(PRICE_ZERO), "P:+0");
        assert_eq!(token_name(encode_price_ticks(-3)), "P:-3");
        assert_eq!(token_name(encode_price_ticks(7)), "P:+7");
        assert_eq!(token_name(SZ_8_15), "SZ:8-15");
    }

    #[test]
    fn display_tokens_formats_sequence() {
        let tokens = vec![BOS, ACT_ADD, SIDE_BID, PRICE_ZERO, SZ_1, COMMIT];
        let s = display_tokens(&tokens);
        assert_eq!(s, "BOS ADD BID P:+0 SZ:1 COMMIT");
    }

    // ── Full tokenizer ──────────────────────────────────────

    fn tick_fixed() -> i64 {
        250_000_000 // 0.25
    }

    fn make_tokenizer() -> MboTokenizer {
        MboTokenizer::new(1, 0.25)
    }

    /// Helper: feed an event with default instrument_id=1.
    fn feed(
        tok: &mut MboTokenizer,
        ts: u64,
        order_id: u64,
        action: char,
        side: char,
        price: i64,
        size: u32,
        flags: u8,
        out: &mut Vec<u16>,
    ) -> usize {
        tok.feed_event(ts, order_id, 1, action, side, price, size, flags, out)
    }

    #[test]
    fn tokenizer_first_events_have_no_ref() {
        // Before the book has both sides, price should be NO_REF
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Add a bid — book is one-sided
        let n = feed(&mut tok, 1000, 100, 'A', 'B', 4500_000_000_000, 10, F_LAST, &mut out);
        assert_eq!(n, 5); // ADD BID NOREF SZ:8-15 COMMIT
        assert_eq!(out[0], ACT_ADD);
        assert_eq!(out[1], SIDE_BID);
        assert_eq!(out[2], PRICE_NO_REF);
        assert_eq!(out[3], encode_size(10));
        assert_eq!(out[4], COMMIT);
    }

    #[test]
    fn tokenizer_two_sided_book_uses_relative_price() {
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Establish two-sided book: bid=4500.00, ask=4500.25
        let bid = 4500_000_000_000i64;
        let ask = 4500_250_000_000i64;

        feed(&mut tok, 1000, 100, 'A', 'B', bid, 10, 0, &mut out);
        feed(&mut tok, 1000, 101, 'A', 'A', ask, 5, F_LAST, &mut out);

        // Both events had NO_REF (first had no book, second had one-sided)
        // First event: no bid or ask → NO_REF
        assert_eq!(out[2], PRICE_NO_REF);
        // Second event: only bid exists, no ask → NO_REF
        assert_eq!(out[6], PRICE_NO_REF);

        // Now book is two-sided. Next event should use relative price.
        out.clear();
        // Add at best bid (4500.00). Mid = 4500.125. Delta = -0.5 ticks → rounds to -1.
        feed(&mut tok, 2000, 102, 'A', 'B', bid, 3, F_LAST, &mut out);
        assert_eq!(out[0], ACT_ADD);
        assert_eq!(out[1], SIDE_BID);
        assert_eq!(out[2], encode_price_ticks(-1)); // P:-1
        assert_eq!(out[3], encode_size(3)); // SZ:2-3
        assert_eq!(out[4], COMMIT);
    }

    #[test]
    fn tokenizer_uses_pre_event_mid() {
        // Critical: price is encoded relative to mid BEFORE the event changes the book.
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Establish book: bid=4500.00, ask=4500.25
        let bid = 4500_000_000_000i64;
        let ask = 4500_250_000_000i64;
        feed(&mut tok, 1000, 100, 'A', 'B', bid, 10, 0, &mut out);
        feed(&mut tok, 1000, 101, 'A', 'A', ask, 5, F_LAST, &mut out);
        out.clear();

        // Now add a new best bid at 4500.25 (improving the bid to the ask level).
        // Pre-event mid = (4500.00 + 4500.25)/2 = 4500.125
        // Event price = 4500.25
        // Delta = 4500.25 - 4500.125 = 0.125 = 0.5 ticks → rounds to +1
        let new_bid = 4500_250_000_000i64;
        feed(&mut tok, 2000, 102, 'A', 'B', new_bid, 5, F_LAST, &mut out);
        assert_eq!(out[2], encode_price_ticks(1)); // P:+1

        // After this event, book has bids at 4500.00 and 4500.25, ask at 4500.25.
        // New mid = (4500.25 + 4500.25) / 2 = 4500.25.
        // A subsequent event at 4500.25 should be P:0 relative to NEW mid.
        out.clear();
        feed(&mut tok, 3000, 103, 'A', 'A', 4500_500_000_000, 3, F_LAST, &mut out);
        // Pre-event mid = (4500.25 + 4500.25) / 2 = 4500.25
        // Event price = 4500.50. Delta = 0.25 = 1 tick → P:+1
        assert_eq!(out[2], encode_price_ticks(1));
    }

    #[test]
    fn tokenizer_commit_on_f_last() {
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Event without F_LAST — no COMMIT
        let n = feed(&mut tok, 1000, 100, 'A', 'B', 4500_000_000_000, 10, 0, &mut out);
        assert_eq!(n, 4); // ADD BID NOREF SZ:8-15 (no COMMIT)
        assert!(!out.contains(&COMMIT));

        // Event with F_LAST — COMMIT appended
        let n = feed(&mut tok, 1000, 101, 'A', 'A', 4501_000_000_000, 5, F_LAST, &mut out);
        assert_eq!(n, 5); // ADD ASK NOREF SZ:4-7 COMMIT
        assert_eq!(*out.last().unwrap(), COMMIT);
    }

    #[test]
    fn tokenizer_multi_event_batch() {
        // Multiple events in one atomic batch (only last has F_LAST)
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Establish book first
        feed(&mut tok, 1000, 100, 'A', 'B', 4500_000_000_000, 10, 0, &mut out);
        feed(&mut tok, 1000, 101, 'A', 'A', 4500_250_000_000, 5, F_LAST, &mut out);
        out.clear();

        // Batch of 3 events: only last has F_LAST
        feed(&mut tok, 2000, 102, 'A', 'B', 4499_750_000_000, 8, 0, &mut out);
        feed(&mut tok, 2000, 103, 'C', 'A', 4500_250_000_000, 5, 0, &mut out);
        feed(&mut tok, 2000, 104, 'A', 'A', 4500_500_000_000, 3, F_LAST, &mut out);

        // Should have: 4 + 4 + 4 + 1(COMMIT) = 13 tokens
        // Only one COMMIT at the end
        assert_eq!(out.len(), 13);
        assert_eq!(out[12], COMMIT);
        let commit_count = out.iter().filter(|&&t| t == COMMIT).count();
        assert_eq!(commit_count, 1);
    }

    #[test]
    fn tokenizer_clear_event() {
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        let n = feed(&mut tok, 1000, 0, 'R', ' ', 0, 0, F_LAST, &mut out);
        assert_eq!(n, 2); // CLEAR COMMIT
        assert_eq!(out[0], ACT_CLEAR);
        assert_eq!(out[1], COMMIT);
    }

    #[test]
    fn tokenizer_skips_other_instruments() {
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Event for instrument 99 — should be skipped
        let n = tok.feed_event(
            1000, 100, 99, 'A', 'B', 4500_000_000_000, 10, F_LAST, &mut out,
        );
        assert_eq!(n, 0);
        assert!(out.is_empty());
        assert_eq!(tok.stats().events_skipped_instrument, 1);
        assert_eq!(tok.stats().events_processed, 0);
    }

    #[test]
    fn tokenizer_trade_event() {
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Establish book
        feed(&mut tok, 1000, 100, 'A', 'B', 4500_000_000_000, 10, 0, &mut out);
        feed(&mut tok, 1000, 101, 'A', 'A', 4500_250_000_000, 5, F_LAST, &mut out);
        out.clear();

        // Trade at the ask (buyer-initiated)
        feed(&mut tok, 2000, 0, 'T', 'A', 4500_250_000_000, 2, F_LAST, &mut out);
        assert_eq!(out[0], ACT_TRADE);
        assert_eq!(out[1], SIDE_ASK);
        // Price at ask = mid + 0.5 ticks → P:+1
        assert_eq!(out[2], encode_price_ticks(1));
        assert_eq!(out[3], encode_size(2)); // SZ:2-3
        assert_eq!(out[4], COMMIT);
    }

    #[test]
    fn tokenizer_fill_event() {
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Establish book
        feed(&mut tok, 1000, 100, 'A', 'B', 4500_000_000_000, 10, 0, &mut out);
        feed(&mut tok, 1000, 101, 'A', 'A', 4500_250_000_000, 5, F_LAST, &mut out);
        out.clear();

        // Fill: order 100 partially filled, 7 remaining
        feed(&mut tok, 2000, 100, 'F', 'B', 4500_000_000_000, 7, F_LAST, &mut out);
        assert_eq!(out[0], ACT_FILL);
        assert_eq!(out[1], SIDE_BID);
        assert_eq!(out[4], COMMIT);
    }

    #[test]
    fn tokenizer_stats_tracking() {
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Establish book (2 events)
        feed(&mut tok, 1000, 100, 'A', 'B', 4500_000_000_000, 10, 0, &mut out);
        feed(&mut tok, 1000, 101, 'A', 'A', 4500_250_000_000, 5, F_LAST, &mut out);

        // Cancel (1 event)
        feed(&mut tok, 2000, 100, 'C', 'B', 4500_000_000_000, 0, F_LAST, &mut out);

        let stats = tok.stats();
        assert_eq!(stats.events_processed, 3);
        assert_eq!(stats.action_counts[0], 2); // ADD
        assert_eq!(stats.action_counts[1], 1); // CANCEL
        assert_eq!(stats.commits, 2); // Two F_LAST events
    }

    #[test]
    fn tokenizer_far_price_tracking() {
        let mut tok = make_tokenizer();
        let mut out = Vec::new();

        // Establish book
        feed(&mut tok, 1000, 100, 'A', 'B', 4500_000_000_000, 10, 0, &mut out);
        feed(&mut tok, 1000, 101, 'A', 'A', 4500_250_000_000, 5, F_LAST, &mut out);

        // Event 100 ticks away from mid (way out of range)
        let far_price = 4500_000_000_000 + 100 * tick_fixed();
        feed(&mut tok, 2000, 102, 'A', 'A', far_price, 1, F_LAST, &mut out);

        assert!(tok.stats().price_far_pos >= 1);
    }

    #[test]
    fn all_valid_tokens_under_vocab_size() {
        // Exhaustively verify no encoder can produce a token >= VOCAB_SIZE
        for action in ['A', 'C', 'M', 'T', 'F', 'R'] {
            if let Some(t) = encode_action(action) {
                assert!((t as usize) < VOCAB_SIZE, "action token {t} >= VOCAB_SIZE");
            }
        }
        for side in ['B', 'A', ' '] {
            let t = encode_side(side);
            assert!((t as usize) < VOCAB_SIZE, "side token {t} >= VOCAB_SIZE");
        }
        for d in -100..=100 {
            let t = encode_price_ticks(d);
            assert!((t as usize) < VOCAB_SIZE, "price token {t} >= VOCAB_SIZE");
        }
        assert!((PRICE_NO_REF as usize) < VOCAB_SIZE);
        for s in [0, 1, 2, 3, 4, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 10000] {
            let t = encode_size(s);
            assert!((t as usize) < VOCAB_SIZE, "size token {t} >= VOCAB_SIZE");
        }
        for special in [PAD, BOS, EOS, COMMIT] {
            assert!((special as usize) < VOCAB_SIZE, "special token {special} >= VOCAB_SIZE");
        }
    }

    #[test]
    fn every_vocab_token_has_a_name() {
        for t in 0..VOCAB_SIZE as u16 {
            let name = token_name(t);
            assert!(!name.starts_with("UNK:"), "token {t} has no name: {name}");
        }
    }
}
