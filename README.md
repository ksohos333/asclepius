# Asclepius

A holistic physician as an MCP server: root-cause oriented, natural-first, and provenance-tracked. Built in Rust for [opencode](https://opencode.ai), [any MCP client](https://modelcontextprotocol.io), and custom harnesses.

Asclepius reads blood work the way an integrative clinician does — against *functional* ranges as well as lab population ranges — ranks root-cause hypotheses from your committed labs, proposes protocols in natural-first order (lifestyle → diet → nutrients → botanicals → physician-required), runs guardrailed n=1 trials, and can verify any medical claim against local knowledge plus PubMed. Every assertion it makes traces back to a lab fact, a cited knowledge-base entry, or an explicitly-labeled hypothesis.

> [!IMPORTANT]
> **Medical disclaimer.** Asclepius is decision-support software for personal health research and clinician collaboration. It is **not medical advice, not a diagnostic device, and not a substitute for emergency or in-person care**. Seed reference ranges and condition cards are approximations, review-dated, and meant to be curated by the user (see [Curation](#curation)). Red-flag symptom patterns are routed to conventional care by design. Use of anything found here is at your own risk; see [LICENSE](LICENSE).

## Why

Standard lab reports say "in range" and stop. Integrative medicine says "optimal" and often overclaims. Forums say everything. Asclepius encodes a third position:

- **Dual ranges** — every analyte is assessed against both the population reference *and* a functional (optimal) range, each carrying a source, a confidence level, and a review date.
- **Evidence tiers, honestly** — PSSD and PAWS literature is largely case series and mechanistic work; the cards say so (tier C/D) rather than dressing hypothesis as consensus. Where evidence is strong, it says that too.
- **Anticonf discipline** — `literature_verify` returns evidence, never verdicts; the caller must compare and cite. An uncited interpretation is treated as a confabulation. No PMIDs are fabricated: unverifiable citations are named and dated for later verification.
- **Root-cause, not symptom suppression** — condition cards map symptoms to mechanisms to drivers to the labs that probe them (e.g. cystic acne → androgens / insulin-IGF-1 / gut-skin axis / fatty-acid balance).
- **Proactive and prophylactic** — `interpret_panel` ends with *untested suggestions*: the next labs that would discriminate between live hypotheses.
- **Guardrails are not optional** — triage red-flag checks, dosing ceilings with reasons, baseline-lab gates on trials, contraindication exclusion, physician-required gating, serotonergic-burden scoring (a specific risk in post-SSRI contexts).

## The clinical loop

```
profile_set ──▶ lab_commit ──▶ interpret_panel ──▶ explore_condition
                    ▲                │                   │
                    │          hypotheses +           propose_protocol
                    │          next tests                  │
              retest labs                                   ▼
                    │                              trial_begin (gated)
                    └────────────── trial_report ◀─ trial_log / status
```

Everything commits to an **append-only provenance ledger**: every fact carries its source, panel, and timestamps; derived values (Vermeulen free testosterone, HOMA-IR) carry their input fact ids and formula version; `fact_lineage` audits any claim back to measured values.

## Tools (22)

| Group | Tools |
|---|---|
| Session | `ping`, `profile_get`, `profile_set` |
| Labs | `lab_normalize` (dry-run), `lab_commit`, `lab_history`, `fact_lineage` |
| Derived | `derived_free_testosterone` (Vermeulen 1999), `derived_homa_ir` (Matthews 1985) |
| Reasoning | `interpret_panel`, `explore_condition`, `hypothesis_register`, `hypothesis_list`, `hypothesis_update` |
| Protocols | `propose_protocol`, `triage_check` |
| Trials | `trial_begin`, `trial_log`, `trial_update_status`, `trial_list`, `trial_report` |
| Evidence | `literature_verify` |

## Setup

Requires a Rust toolchain (1.85+).

```sh
git clone https://github.com/ksohos333/asclepius.git
cd asclepius
cargo build --release
```

### opencode

In `~/.config/opencode/opencode.json` (or `.opencode/opencode.json`):

```json
{
  "mcp": {
    "asclepius": {
      "type": "local",
      "command": ["/path/to/asclepius/target/release/asclepius-mcp"],
      "environment": {
        "ASCLEPIUS_PACKS": "/path/to/asclepius/content-packs"
      },
      "enabled": true,
      "timeout": 60000
    }
  }
}
```

Restart opencode, then start with: *“asclepius: profile_set — male, 30, conditions [cystic acne], goals [clear skin]”* followed by *“lab_commit”* with your values.

### Any MCP client / custom harness

Spawn `asclepius-mcp` over stdio with `ASCLEPIUS_PACKS` pointing at `content-packs/`. That's the whole contract.

## Configuration

| Env var | Default | Meaning |
|---|---|---|
| `ASCLEPIUS_PACKS` | `./content-packs`, else `$ASCLEPIUS_HOME/content-packs` | Content-pack root |
| `ASCLEPIUS_HOME` | `~/.local/share/asclepius` | Data home (ledger lives at `$ASCLEPIUS_HOME/asclepius.db`) |
| `ASCLEPIUS_PUBMED` | unset (off) | `1` enables PubMed egress in `literature_verify` — **the claim text is sent to NCBI eutils** |
| `ASCLEPIUS_PUBMED_KEY` | unset | Optional NCBI API key |
| `ASCLEPIUS_BRIDGE` | unset (off) | `1` enables status posts to the anticonf bridge log |
| `ASCLEPIUS_BRIDGE_PATH` | `~/.local/share/anticonf/data/bridge.jsonl` | Bridge log location |

Network egress is **off by default**. The only egress is PubMed lookups, behind an explicit flag, with abstracts cached locally in the ledger.

## Curation

The medical knowledge lives in versioned TOML content packs, not compiled into the binary:

```
content-packs/
├── analytes/       # id, aliases, units, molar mass, plausibility bounds
├── ranges/         # population + functional ranges w/ source, confidence, review_by
├── rules/          # declarative interpretation rules (analyte, kind, status) → finding
├── conditions/     # root-cause cards: mechanisms, biomarkers, honest tier notes
├── interventions/  # dosing, ceilings, contraindications, interactions, flags
└── triage/         # red-flag patterns → urgency
```

Everything is data: edit a pack, restart the server, done. Seeds shipped here are approximations marked with confidence levels and `review_by` dates — the intended workflow is that **you** curate them (and commit your curation) until the review queue is empty. `literature_verify` with `ASCLEPIUS_PUBMED=1` fetches real abstracts to check or upgrade any citation.

Range scoping supports sex and age bands. Rules use AND semantics over `(analyte, range_kind, status)` conditions, so they survive range curation without edits.

## Architecture

```
crates/
├── asclepius-core/   # deterministic engine: units (mass↔molar bridges), analyte
│                     # registry w/ alias resolution, dual-range engine, panel
│                     # normalization + plausibility gates, Vermeulen/HOMA-IR,
│                     # rules interpreter, condition search, intervention
│                     # proposal + guardrails, SQLite ledger (append-only,
│                     # provenance on every row), PubMed client, bridge
└── asclepius-mcp/    # thin rmcp (stdio) shell exposing the engine
```

Unit-conversion math and formulas are pure functions with unit tests against published worked examples (e.g. Vermeulen free-T ≈ 5.2 ng/dL for TT 500 ng/dL / SHBG 30 nmol/L / albumin 4.3 g/dL). The ledger is single-writer: the MCP server owns the database file.

## Status

v0.1.0 — the full loop works end to end (verified with sequential-client probes: commit → interpret → protocol → gated trial → retest → delta report). Seeded knowledge covers the endocrine-metabolic panel, cystic acne root causes, PSSD and PAWS. The curation queue is long and that is by design: this is a knowledge base that earns trust one reviewed claim at a time.

## License

[MIT](LICENSE) — with the medical disclaimer above, which the license does not waive.
