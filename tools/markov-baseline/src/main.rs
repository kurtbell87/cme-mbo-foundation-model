//! Markov baseline: measure perplexity at orders 1–20 on tokenized MBO data.
//!
//! This is the null hypothesis for the transformer grammar-learning experiment.
//! If a transformer can't beat order-5 Markov, there's no compositional structure
//! worth learning. If it crushes it, the grammar is real.
//!
//! The perplexity curve shape across orders 1–20 reveals the characteristic
//! dependency length of the LOB grammar, which directly informs architecture
//! selection (Mamba vs sparse attention vs hierarchical).
//!
//! **Per-token-type breakdown** is the key output: aggregate perplexity hides
//! that COMMIT is trivially predictable (perplexity ~1) while the size token
//! after TRADE might have perplexity 15. The breakdown shows where the actual
//! information content lives.

use std::fs::File;
use std::io::Read;

use anyhow::{bail, Context, Result};
use clap::Parser;
use rustc_hash::FxHashMap;

use mbo_tokenizer::{
    VOCAB_SIZE,
    ACT_ADD, ACT_CLEAR,
    SIDE_BID, SIDE_NONE,
    PRICE_FAR_NEG, PRICE_NO_REF,
    SZ_0, SZ_128_PLUS,
};

#[derive(Parser, Debug)]
#[command(name = "markov-baseline")]
#[command(about = "Markov chain perplexity baseline for tokenized MBO data")]
struct Args {
    /// Input binary token file (u16 little-endian, from mbo-tokenize).
    #[arg(required = true)]
    input: String,

    /// Maximum Markov order to evaluate (1..=max_order).
    #[arg(long, default_value = "20")]
    max_order: usize,

    /// Fraction of data to use for training (rest is held-out test).
    #[arg(long, default_value = "0.8")]
    train_frac: f64,

    /// Laplace smoothing parameter (added to all counts).
    #[arg(long, default_value = "1.0")]
    alpha: f64,

    /// Print per-token-type perplexity breakdown for this order.
    /// If 0, prints breakdown for the best (lowest perplexity) order.
    #[arg(long, default_value = "0")]
    breakdown_order: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.max_order == 0 || args.max_order > 30 {
        bail!("max_order must be in [1, 30]");
    }
    if args.train_frac <= 0.0 || args.train_frac >= 1.0 {
        bail!("train_frac must be in (0, 1)");
    }

    // ── Load tokens ─────────────────────────────────────────
    eprintln!("[1] Loading tokens from {}...", args.input);
    let tokens = load_tokens(&args.input)?;
    eprintln!("    {} tokens loaded", tokens.len());

    if tokens.len() < 1000 {
        bail!("Need at least 1000 tokens, got {}", tokens.len());
    }

    // ── Train/test split ────────────────────────────────────
    let split = (tokens.len() as f64 * args.train_frac) as usize;
    let train = &tokens[..split];
    let test = &tokens[split..];
    eprintln!("[2] Split: {} train, {} test ({:.0}%/{:.0}%)",
        train.len(), test.len(),
        args.train_frac * 100.0, (1.0 - args.train_frac) * 100.0);

    // ── Classify tokens for per-type breakdown ──────────────
    let token_classes = build_token_classes();

    // ── Sweep orders 1..=max_order ──────────────────────────
    eprintln!("[3] Computing Markov perplexity for orders 1..={}...", args.max_order);
    eprintln!();

    let mut results: Vec<OrderResult> = Vec::new();
    let mut best_order = 1;
    let mut best_ppl = f64::MAX;

    // Header
    eprintln!("{:>5}  {:>12}  {:>12}  {:>12}  {:>12}",
        "order", "perplexity", "entropy", "unique_ctx", "test_toks");
    eprintln!("{}", "-".repeat(60));

    for order in 1..=args.max_order {
        let result = evaluate_order(train, test, order, args.alpha, &token_classes);

        eprintln!("{:>5}  {:>12.4}  {:>12.4}  {:>12}  {:>12}",
            order, result.perplexity, result.entropy_bits,
            result.unique_contexts, result.test_tokens);

        if result.perplexity < best_ppl {
            best_ppl = result.perplexity;
            best_order = order;
        }

        results.push(result);
    }

    eprintln!();
    eprintln!("Best: order={best_order} perplexity={best_ppl:.4}");

    // ── Perplexity curve (ASCII) ────────────────────────────
    eprintln!();
    print_ascii_curve(&results);

    // ── Per-token-type breakdown ────────────────────────────
    let breakdown_order = if args.breakdown_order == 0 {
        best_order
    } else {
        args.breakdown_order
    };

    if breakdown_order <= args.max_order {
        eprintln!();
        eprintln!("=== Per-token-type perplexity breakdown (order={breakdown_order}) ===");
        eprintln!();
        print_token_type_breakdown(&results[breakdown_order - 1]);
    }

    // ── Dependency length analysis ──────────────────────────
    eprintln!();
    analyze_dependency_length(&results);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// Core types
// ═══════════════════════════════════════════════════════════════

struct OrderResult {
    order: usize,
    perplexity: f64,
    entropy_bits: f64,
    unique_contexts: usize,
    test_tokens: usize,
    /// Per token class: (class_name, perplexity, count, entropy_bits)
    per_class: Vec<(String, f64, u64, f64)>,
}

/// Token class for per-type breakdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TokenClass {
    Special,  // PAD, BOS, EOS, COMMIT
    Action,   // ADD, CANCEL, MODIFY, TRADE, FILL, CLEAR
    Side,     // BID, ASK, NONE
    Price,    // all price tokens
    Size,     // all size tokens
}

impl TokenClass {
    fn name(self) -> &'static str {
        match self {
            TokenClass::Special => "SPECIAL",
            TokenClass::Action => "ACTION",
            TokenClass::Side => "SIDE",
            TokenClass::Price => "PRICE",
            TokenClass::Size => "SIZE",
        }
    }
}

fn build_token_classes() -> Vec<TokenClass> {
    let mut classes = vec![TokenClass::Special; VOCAB_SIZE];
    for t in ACT_ADD..=ACT_CLEAR {
        classes[t as usize] = TokenClass::Action;
    }
    for t in SIDE_BID..=SIDE_NONE {
        classes[t as usize] = TokenClass::Side;
    }
    for t in PRICE_FAR_NEG..=PRICE_NO_REF {
        classes[t as usize] = TokenClass::Price;
    }
    for t in SZ_0..=SZ_128_PLUS {
        classes[t as usize] = TokenClass::Size;
    }
    classes
}

// ═══════════════════════════════════════════════════════════════
// Markov model
// ═══════════════════════════════════════════════════════════════

/// Context key: up to 30 tokens packed into a fixed-size array.
/// Using a small-vec approach with length prefix for hashing.
#[derive(Clone, Eq, PartialEq, Hash)]
struct ContextKey {
    len: u8,
    tokens: [u16; 30],
}

impl ContextKey {
    fn from_slice(s: &[u16]) -> Self {
        debug_assert!(s.len() <= 30);
        let mut tokens = [0u16; 30];
        tokens[..s.len()].copy_from_slice(s);
        Self {
            len: s.len() as u8,
            tokens,
        }
    }
}

/// Count table: context → [count per token in vocab]
struct MarkovModel {
    /// context → counts array (indexed by next token)
    counts: FxHashMap<ContextKey, Vec<u32>>,
    alpha: f64,
}

impl MarkovModel {
    fn new(alpha: f64) -> Self {
        Self {
            counts: FxHashMap::default(),
            alpha,
        }
    }

    fn train(&mut self, tokens: &[u16], order: usize) {
        if tokens.len() <= order {
            return;
        }
        for i in order..tokens.len() {
            let ctx = ContextKey::from_slice(&tokens[i - order..i]);
            let next = tokens[i] as usize;
            let entry = self.counts
                .entry(ctx)
                .or_insert_with(|| vec![0u32; VOCAB_SIZE]);
            entry[next] += 1;
        }
    }

    /// Compute log probability of next token given context.
    /// Returns log2(P(next | context)) using Laplace smoothing.
    fn log_prob(&self, context: &[u16], next: u16) -> f64 {
        let ctx = ContextKey::from_slice(context);
        let next_idx = next as usize;

        match self.counts.get(&ctx) {
            Some(counts) => {
                let total: f64 = counts.iter().map(|&c| c as f64).sum::<f64>()
                    + self.alpha * VOCAB_SIZE as f64;
                let count = counts[next_idx] as f64 + self.alpha;
                (count / total).log2()
            }
            None => {
                // Unseen context: uniform distribution with smoothing
                (1.0 / VOCAB_SIZE as f64).log2()
            }
        }
    }

    fn unique_contexts(&self) -> usize {
        self.counts.len()
    }
}

// ═══════════════════════════════════════════════════════════════
// Evaluation
// ═══════════════════════════════════════════════════════════════

fn evaluate_order(
    train: &[u16],
    test: &[u16],
    order: usize,
    alpha: f64,
    token_classes: &[TokenClass],
) -> OrderResult {
    // Train
    let mut model = MarkovModel::new(alpha);
    model.train(train, order);

    // Evaluate on test set
    let mut total_log_prob = 0.0f64;
    let mut n_tokens = 0u64;

    // Per-class accumulators
    let mut class_log_prob: FxHashMap<TokenClass, f64> = FxHashMap::default();
    let mut class_count: FxHashMap<TokenClass, u64> = FxHashMap::default();

    if test.len() > order {
        for i in order..test.len() {
            let context = &test[i - order..i];
            let next = test[i];
            let lp = model.log_prob(context, next);

            total_log_prob += lp;
            n_tokens += 1;

            let class = token_classes[next as usize];
            *class_log_prob.entry(class).or_default() += lp;
            *class_count.entry(class).or_default() += 1;
        }
    }

    let entropy_bits = if n_tokens > 0 {
        -total_log_prob / n_tokens as f64
    } else {
        0.0
    };
    let perplexity = 2.0f64.powf(entropy_bits);

    // Per-class results
    let mut per_class = Vec::new();
    for class in &[TokenClass::Special, TokenClass::Action, TokenClass::Side,
                   TokenClass::Price, TokenClass::Size] {
        let lp = class_log_prob.get(class).copied().unwrap_or(0.0);
        let cnt = class_count.get(class).copied().unwrap_or(0);
        let ent = if cnt > 0 { -lp / cnt as f64 } else { 0.0 };
        let ppl = 2.0f64.powf(ent);
        per_class.push((class.name().to_string(), ppl, cnt, ent));
    }

    OrderResult {
        order,
        perplexity,
        entropy_bits,
        unique_contexts: model.unique_contexts(),
        test_tokens: n_tokens as usize,
        per_class,
    }
}

// ═══════════════════════════════════════════════════════════════
// Output formatting
// ═══════════════════════════════════════════════════════════════

fn print_token_type_breakdown(result: &OrderResult) {
    eprintln!("{:<12} {:>12} {:>12} {:>12}",
        "class", "perplexity", "entropy", "count");
    eprintln!("{}", "-".repeat(52));

    for (name, ppl, count, entropy) in &result.per_class {
        eprintln!("{:<12} {:>12.4} {:>12.4} {:>12}",
            name, ppl, entropy, count);
    }

    eprintln!();
    eprintln!("{:<12} {:>12.4} {:>12.4} {:>12}",
        "AGGREGATE", result.perplexity, result.entropy_bits, result.test_tokens);

    // Also show individual token perplexities for the most interesting tokens
    eprintln!();
    eprintln!("Interpretation:");
    for (name, ppl, count, _) in &result.per_class {
        if *count == 0 { continue; }
        let predictability = if *ppl < 1.5 {
            "trivially predictable"
        } else if *ppl < 3.0 {
            "mostly predictable"
        } else if *ppl < 8.0 {
            "moderate uncertainty"
        } else if *ppl < 20.0 {
            "high uncertainty"
        } else {
            "near-uniform (very uncertain)"
        };
        eprintln!("  {:<12} ppl={:>8.2} → {}", name, ppl, predictability);
    }
}

fn print_ascii_curve(results: &[OrderResult]) {
    eprintln!("=== Perplexity curve ===");
    eprintln!();

    let max_ppl = results.iter().map(|r| r.perplexity).fold(0.0f64, f64::max);
    let min_ppl = results.iter().map(|r| r.perplexity).fold(f64::MAX, f64::min);
    let width = 50;

    for r in results {
        let bar_len = if max_ppl > min_ppl {
            ((r.perplexity - min_ppl) / (max_ppl - min_ppl) * width as f64) as usize
        } else {
            width / 2
        };
        let bar_len = bar_len.max(1);
        let bar: String = "█".repeat(bar_len);
        eprintln!("  {:>2} │{:<50} {:.4}",
            r.order, bar, r.perplexity);
    }
    eprintln!("     └{}", "─".repeat(50));
}

fn analyze_dependency_length(results: &[OrderResult]) {
    eprintln!("=== Dependency length analysis ===");
    eprintln!();

    if results.len() < 2 {
        eprintln!("  Need at least 2 orders for analysis.");
        return;
    }

    // Compute relative improvement at each step
    eprintln!("{:>5}  {:>12}  {:>15}  {:>15}",
        "order", "perplexity", "Δ ppl", "Δ ppl %");
    eprintln!("{}", "-".repeat(52));

    let base_ppl = results[0].perplexity;
    let mut prev_ppl = base_ppl;
    let mut last_significant_order = 1;

    for r in results {
        let delta = r.perplexity - prev_ppl;
        let delta_pct = if prev_ppl > 0.0 {
            delta / prev_ppl * 100.0
        } else {
            0.0
        };

        eprintln!("{:>5}  {:>12.4}  {:>+15.4}  {:>+14.2}%",
            r.order, r.perplexity, delta, delta_pct);

        // "Significant" = more than 0.5% relative improvement
        if delta_pct < -0.5 {
            last_significant_order = r.order;
        }

        prev_ppl = r.perplexity;
    }

    let total_improvement = (base_ppl - results.last().unwrap().perplexity) / base_ppl * 100.0;
    let first_to_best = (base_ppl - results.iter().map(|r| r.perplexity).fold(f64::MAX, f64::min)) / base_ppl * 100.0;

    eprintln!();
    eprintln!("Total improvement (order 1 → {}): {:.2}%", results.len(), total_improvement);
    eprintln!("Improvement to best: {:.2}%", first_to_best);
    eprintln!("Last order with >0.5% improvement: {last_significant_order}");

    let still_improving = results.len() >= 2 && {
        let last = &results[results.len() - 1];
        let prev = &results[results.len() - 2];
        (prev.perplexity - last.perplexity) / prev.perplexity > 0.005
    };

    if still_improving {
        eprintln!();
        eprintln!("⚠ Perplexity still dropping at order {}.", results.len());
        eprintln!("  Long-range dependencies likely exist.");
        eprintln!("  Consider: sparse attention > Mamba for architecture choice.");
    } else {
        eprintln!();
        eprintln!("  Perplexity has plateaued by order {last_significant_order}.");
        eprintln!("  Dependencies are primarily local ({last_significant_order}-gram range).");
        if last_significant_order <= 5 {
            eprintln!("  Mamba's recurrent state is likely sufficient.");
        } else {
            eprintln!("  Medium-range dependencies. Consider hybrid architecture.");
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// I/O
// ═══════════════════════════════════════════════════════════════

fn load_tokens(path: &str) -> Result<Vec<u16>> {
    let mut file = File::open(path).context("Failed to open token file")?;
    let file_len = file.metadata()?.len() as usize;

    if file_len % 2 != 0 {
        bail!("Token file size {} is not a multiple of 2 (expected u16 values)", file_len);
    }

    let n_tokens = file_len / 2;
    let mut buf = vec![0u8; file_len];
    file.read_exact(&mut buf)?;

    let tokens: Vec<u16> = buf
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    assert_eq!(tokens.len(), n_tokens);

    // Validate all tokens are in vocab range
    let invalid = tokens.iter().filter(|&&t| t as usize >= VOCAB_SIZE).count();
    if invalid > 0 {
        bail!("{invalid} tokens out of vocab range [0, {VOCAB_SIZE})");
    }

    Ok(tokens)
}
