# Research Index — MBO Deep Learning

Last updated: 2026-03-11

**Start here:** `.kit/RESEARCH_LOG.md` — consolidated findings, what works, what doesn't, path forward.
**Open questions:** `.kit/QUESTIONS.md` — prioritized research agenda.

---

## Completed Threads (Negative)

### Thread 01: Bar-Level CPCV — REFUTED
- 5s bar features + triple-barrier + 2-stage XGBoost
- Overlapping Sharpe 14.21 → serial Sharpe -18.26
- 45/45 CPCV folds negative under serial execution
- Root cause: bar aggregation destroys intra-bar dynamics, mid-price entry is fictional

### Thread 02: Tick-Level Serial Re-simulation — REFUTED
- Re-simulated Thread 01 signals at tick resolution
- Win rate 26.6% matches null (S/(T+S) = 26.9%)
- Barrier sweep (19:7, 19:19, time-exit) all confirm null
- 12-31% of labels flip between bar and tick resolution

### Thread 03: Event-Level LOB Probability — REFUTED
- 44 LOB features, event-level, bid/ask entry, probability regression
- Gate test: all univariate signals near-null across 11 geometries
- Full CPCV (10:5, 45 folds): **0/45 positive**. Mean expectancy -$1.37/trade
- Static book snapshots do not predict short-term direction

### Thread 04: Seq-Features CPCV — REFUTED
- 22 inter-episode BBO dynamics features + OFI-directed labels
- 0/45 negative folds BUT massive trade autocorrelation (Ljung-Box Q=184K)
- Root cause: evaluating slow EMA signal on fast BBO-change grid
- Key insight: evaluation grid must match signal timescale

### Thread 06: Cooldown Sweep — REFUTED
- Tested Thread 04 seq-features with cooldown values 0–422
- All 45/45 folds negative across every cooldown. Expectancy ~$-1.75/trade
- Confirms autocorrelation was symptom, not disease — no real signal in seq-features
- **Exhausts XGBoost on hand-engineered MBO features** (4 feature sets, all null)

---

## Active Thread

### Thread 05: MBO Grammar — Transformer Pretraining (ACTIVE)
- 126-token MBO vocabulary, 3.93B tokens from full-year MES 2022
- **Phase 0:** Tokenizer + sidecars (tokens.bin, .book_state, .mids) — DONE
- **Phase 1 (Gate 1):** LM perplexity 1.864 vs Markov-5 1.984 (6.1% improvement) — **PASSED**
- **Gate 2 (frozen probe):** Pretrained representations show no directional advantage over raw one-hot — NEGATIVE
- **Phase 2 (Gate 1.5):** Dual-head book state reconstruction — size acc 67.4% (vs 23.6% baseline), spread NM 84.4% — **PASSED**
- **Phase 3 (directional signal check):** B (pretrained+recon) vs C (random) on next BBO change direction — **READY**
- Code: `research/04-mbo-grammar/` and `../mbo-tokenization/research/04-mbo-grammar/`

---

## Data on S3

- `s3://kenoma-labs-research/data/MES-MBO-2022/` — 312 .dbn.zst files, 49.2 GB (raw MBO)
- `s3://kenoma-labs-research/cloud-runs/mbo-grammar/` — tokens.bin (7.3 GiB), .book_state (31.1 GiB), .mids (10.4 GiB), .meta.json
- `s3://kenoma-labs-research/runs/mbo-grammar-phase1-20260308T023435Z/` — Phase 1 checkpoint
- `s3://kenoma-labs-research/runs/mbo-grammar-phase2-20260310T020658Z/results/` — Phase 2 checkpoint + gate results

---

## Result Files (Local, Historical Reference)

- `02-tick-level-serial/results/cpcv-performance-report.md` — Definitive Thread 02 analysis
- `02-tick-level-serial/results/ec2-barrier-sweep/` — Barrier geometry sensitivity (6 JSON)
- `03-event-lob-probability/results/gate-test-report.json` — Univariate signal lift analysis
- Thread 03 CPCV results: S3 purged, summary in `.kit/RESEARCH_LOG.md`
- Thread 04/06 results: S3 only, summaries in `.kit/RESEARCH_LOG.md`
