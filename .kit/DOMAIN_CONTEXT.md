# Domain Context

Domain knowledge, Mathlib mappings, and notation conventions for this project.

## Domain Description
<!-- What mathematical domain does this project cover? -->
<!-- e.g., "Order theory for limit order book modeling" -->


## Mathlib Type Mappings

| Domain Concept | Mathlib Type | Module |
|---------------|-------------|--------|
| _[concept]_ | `Type` | `Mathlib.Module` |

## Notation Table

| Symbol | Lean4 | Meaning |
|--------|-------|---------|
| | | |

## Key Mathlib Lemmas

| Lemma | Module | Used For |
|-------|--------|----------|
| | | |

## Project-Specific Conventions
<!-- Naming conventions, proof style preferences, etc. -->

- Follow Mathlib naming conventions (`snake_case` for definitions, descriptive theorem names)
- Use `namespace` to organize related definitions
- Prefer `structure` over `class` for concrete mathematical objects
- Use Mathlib typeclasses for abstract algebraic structures

## Known Limitations
<!-- Things Mathlib doesn't have that we need to build ourselves -->

## DOES NOT APPLY
<!-- Record failed approaches here during PROVE phase.
     Each entry should explain WHY the lemma/approach doesn't work.
     This prevents future revision cycles from re-attempting known-bad approaches. -->
<!-- Example:
- MeasureTheory.StronglyMeasurable.integral_condexp: requires [TopologicalSpace α], our α is bare ℕ → ℝ
- MeasureTheory.Stopping.isStoppingTime_min: only for ℕ-indexed filtrations, we need ℝ-indexed
-->
