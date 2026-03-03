# Constructions Queue

Program mode reads this file to auto-advance through mathematical constructions.

## Priority Queue

| Priority | Construction | Spec File | Status | Depends On | Notes |
|----------|-------------|-----------|--------|------------|-------|
| P1 | _[Name of construction]_ | `specs/[name].md` | Not started | — | _[Brief description]_ |
| P2 | _[Name]_ | `specs/[name].md` | Not started | — | |
| P3 | _[Name]_ | `specs/[name].md` | Not started | P1 | |

### Status Values
- **Not started** — spec not yet written
- **Specified** — spec complete, ready for construction
- **Constructed** — informal math done, ready for formalization
- **Formalized** — .lean files written (all sorry)
- **Proved** — all sorrys eliminated
- **Audited** — passed audit, logged
- **Revision** — needs revision (see REVISION.md)
- **Blocked** — blocked on dependency

---

## Completed

| Construction | Spec File | Date Completed | Theorems |
|-------------|-----------|----------------|----------|
| | | | |

---

## Dependencies
<!-- If construction B depends on construction A, note it here -->
<!-- e.g., "P2 depends on P1 (uses the lattice structure from P1)" -->
