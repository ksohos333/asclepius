# Contributing to Asclepius

Asclepius is a knowledge engine wearing a safety harness: nearly everything a
contributor adds is **data** — TOML content packs that the deterministic
engine loads, validates, hashes, and cites. Code changes are rare by design;
curation is the contribution that matters.

## The one rule

Every entry is a claim: it carries a `tier`, `sources`, and a `review_by`
date, and it must survive the question *"how do you know?"* If the honest
answer is "emerging literature", the tier is C. If it is "mechanistic
reasoning", C or D. If it is "community reports", D with an approximate
count. **Confidently-worded weak evidence is the failure mode this project
exists to prevent** — in either direction, including dismissing patient
experience that simply lacks trials.

## Pack domains

| Domain | What lives there | Validation the gate enforces |
|---|---|---|
| `analytes/` | lab analyte definitions (id, units, molar mass, plausibility bounds) | unique ids |
| `ranges/` | population + functional ranges | analyte must exist; unit coherence |
| `rules/` | interpretation rules over range statuses | conditions reference known analytes |
| `conditions/` | condition cards (summary, mechanisms, biomarkers, evidence floor) | biomarkers resolve or become visible gaps |
| `interventions/` | interventions (dosing, ceilings, contraindications, interactions, flags) | targets reference known conditions |
| `triage/` | red-flag patterns → urgency | unique ids |
| `causal/` | causal graphs: nodes (root_cause/mechanism/symptom, `conditions` list, optional `mechanism_family`, lab probes), weighted tiered edges | edges resolve; graphs acyclic; conditions exist |
| `treatment-responses/` | observable response patterns → score shifts on causal nodes | effects reference causal nodes |
| `tests/` | investigation tests (discriminates node + resolution, cost band, caveats) | discriminates reference causal nodes |
| `regions/` | layered geography: `_base` < bloc (`eu`) < country (`gr`, `us`, `de`); test routes, legal classes, physician routes with bilingual scripts, purchasable products | routes/classes reference known tests/interventions/conditions |
| `symptoms/` | patient-reported outcome scales (single-item, instruments) | conditions exist; unique items |
| `community-signals/` | structured hurt/helped reports (tier D, approximate counts) | items resolve; direction sane |
| `corpus/` | key papers (tier-tagged, review-dated; PMIDs only when verified) | unique ids |

A file that fails validation is **excluded and reported in `ping`** — the
server starts degraded rather than wrong. Forward references (a rule
suggesting an analyte nobody defined yet) surface as visible gaps, not
errors.

## Adding a country region

Copy `regions/gr.toml` (or `us.toml`/`de.toml`) to `<cc>.toml`, set the
`[region]` header (country code, bloc if any, name, locale), and curate:

- `test_route` entries for the tests your country can actually obtain, with
  honest cost bands and whether a referral is needed
- `legal_class` entries for interventions (classes are data, not legal advice)
- `physician_route` entries with request scripts in the local language and
  English — the scripts are the feature people will feel
- `product` entries mapping interventions to real purchasable products,
  with label-honest `disclosure` and `sources` (label-sourced data says so)

## Curation discipline

- **Tiers**: A (replicated strong evidence) → B (solid human evidence) →
  C (emerging/mechanistic) → D (case reports/community/hypothesis). When
  unsure, go lower and say why in `sources`.
- **review_by**: near-term dates for weak tiers. Overdue entries still fire
  but every affected claim is flagged `stale` — the pressure to re-review is
  the point.
- **Sources**: author-year descriptions are fine; exact PMIDs only when you
  have verified them. Unverifiable citations stay descriptive, never
  invented.
- **No medical advice in packs**: dosage ceilings, contraindications, and
  request scripts are decision support for a conversation with a clinician,
  not instructions to act alone.
- **Community data** goes in `community-signals/` with approximate counts and
  review dates — never presented as trial evidence, never silently ignored.

## Workflow

1. Edit or add pack files.
2. `cargo test` (the corruption-detection suite guards the engine).
3. Run the server and call `ping` — excluded files and knowledge gaps are
   reported there.
4. PR with a description of *what claim you are making and how you know it*.

## Medical disclaimer

Asclepius is decision-support software for personal health research and
clinician collaboration. It is not medical advice, not a diagnostic device,
and not a substitute for emergency or in-person care. Contributors curate
claims, not prescriptions.
