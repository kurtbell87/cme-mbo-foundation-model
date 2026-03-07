# Research Log — MBO Tokenization

Cumulative findings from all experiments. Each entry is a concise summary.
Read this file FIRST when starting any new research task.

---

## [baseline-001-markov-sweep] — CONFIRMED (structure exists)
**Date:** 2026-03-06
**Hypothesis:** MBO token sequences have learnable structure beyond unigram.
**Key result:** Order-5 Markov achieves ppl 1.98 (0.99 bits/token) on 74.5M MES tokens. Stream is 86% redundant given 5 tokens of context.
**Per-token-type breakdown:**
- SPECIAL (COMMIT/BOS/EOS): ppl 1.03 — trivially predictable, should be masked from training loss
- ACTION: ppl 2.77 (1.47 bits) — core prediction task, "what happens next"
- SIDE: ppl 1.78 (0.83 bits) — partially predictable from action context
- PRICE: ppl 2.75 (1.46 bits) — nearly as uncertain as action, likely benefits most from longer context
- SIZE: ppl 2.03 (1.02 bits) — moderate, dominated by size=1 in retail MES

**Perplexity curve:** Drops order 1→5, then rises (data sparsity, not structural plateau). 18.9M unique contexts at order 20 for 59.6M training tokens — Markov can't test long-range structure.
**Lesson:** Structure exists and is strong. The Markov model hits its representational ceiling at order 5 — a transformer's distributed representations should generalize where Markov's memorized n-grams can't. But beating Markov on perplexity is the easy gate. The hard gate is directional encoding (Gate 2).
**Next:** Build transformer, test Gate 1 (ppl < 1.98), then Gate 2 (linear probe for direction).
**Details:** `tools/markov-baseline/`, run on `glbx-mdp3-20250102.mbo.dbn.zst`

---

## [infra-001-tokenizer] — COMPLETE
**Date:** 2026-03-06
**What:** MBO event tokenizer — 126-token vocabulary, multi-token encoding.
**Design:** Each event → [ACTION] [SIDE] [PRICE_REL] [SIZE] + [COMMIT] at F_LAST boundaries. Price relative to pre-event mid in integer ticks (±50 range). Size log₂-quantized (9 buckets).
**Validation:** 15.3M MES events → 74.5M tokens. 94.5% prices in range. 4.88 tokens/event. 31 unit tests.
**Key design choice:** Multi-token encoding preserves compositional structure (ADD+BID shares representation with ADD+ASK). Single-token-per-event would have 2688 vocab but lose compositionality.
**Details:** `crates/mbo-tokenizer/`, `tools/mbo-tokenize/`
