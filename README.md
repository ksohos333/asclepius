# Asclepius

A holistic physician as an MCP server: root-cause oriented, natural-first, and provenance-tracked. Built in Rust for [opencode](https://opencode.ai), [any MCP client](https://modelcontextprotocol.io), and custom harnesses.

Asclepius reads blood work the way an integrative clinician does — against *functional* ranges as well as lab population ranges — ranks root-cause hypotheses from your committed labs, proposes protocols in natural-first order (lifestyle → diet → nutrients → botanicals → physician-required), runs guardrailed n=1 trials, and can verify any medical claim against local knowledge plus PubMed. Every tool response is an **assertion envelope**: a list of claims, each bound to evidence ids (ledger facts, pack content hashes, knowledge ids), recomputed by an independent in-process verifier before serialization. A claim without evidence cannot be emitted; an envelope that fails verification is refused, not sent.

> [!IMPORTANT]
> **Medical disclaimer.** Asclepius is decision-support software for personal health research and clinician collaboration. It is **not medical advice, not a diagnostic device, and not a substitute for emergency or in-person care**. Seed reference ranges and condition cards are approximations, review-dated, and meant to be curated by the user (see [Curation](#curation)). Red-flag symptom patterns are routed to conventional care by design. Use of anything found here is at your own risk; see [LICENSE](LICENSE).

## Why

Standard lab reports say "in range" and stop. Integrative medicine says "optimal" and often overclaims. Forums say everything. Asclepius encodes a third position:

- **Dual ranges** — every analyte is assessed against both the population reference *and* a functional (optimal) range, each carrying a source, a confidence level, and a review date.
- **Evidence tiers, honestly** — PSSD and PAWS literature is largely case series and mechanistic work; the cards say so (tier C/D) rather than dressing hypothesis as consensus. Where evidence is strong, it says that too.
- **Anticonf discipline** — `literature_verify` returns evidence, never verdicts; the caller must compare and cite. An uncited interpretation is treated as a confabulation. No PMIDs are fabricated: unverifiable citations are named and dated for later verification.
- **The engine cannot lie by construction** — every response is an envelope of claims; the only prose a claim carries is quoted pack text referenced by id. A separate verifier module (`verify.rs`, sharing no code with the constructors) re-fetches every fact, re-runs every formula (Vermeulen, HOMA-IR, deltas) from published definitions, and refuses any envelope whose numbers it cannot re-derive. The corruption-detection test suite is the merge gate.
- **Load gate before knowledge** — packs are hashed (sha256), parsed, and cross-validated at startup. A broken file is excluded and reported in `ping` (degraded mode, never silent). Forward references — rules suggesting tests nobody defined yet, cards listing undefined biomarkers — are surfaced as *knowledge gaps* in `ping`, because they are curation debt, not corruption. Overdue `review_by` dates don't hide either: affected claims carry `stale: true` naming the missed date, and every envelope pins the content hashes it was computed against.
- **Root-cause, not symptom suppression** — condition cards map symptoms to mechanisms to drivers to the labs that probe them (e.g. cystic acne → androgens / insulin-IGF-1 / gut-skin axis / fatty-acid balance).
- **Proactive and prophylactic** — `interpret_panel` ends with *untested suggestions*: the next labs that would discriminate between live hypotheses.
- **Guardrails are not optional** — triage red-flag checks, dosing ceilings with reasons, baseline-lab gates on trials, contraindication exclusion, physician-required gating, serotonergic-burden scoring (a specific risk in post-SSRI contexts).
- **Abstention is an answer** — when the ledger is empty or the knowledge runs out, tools return abstentions that say what is missing instead of filling the gap with fluent nothing.

## The clinical loop

```
profile_set ──▶ lab_commit ──▶ interpret_panel ──▶ explore_condition
                    ▲                │                   │
                    │          hypotheses +           propose_protocol
                    │          next tests                  │
              retest labs                                   ▼
                    │                              trial_begin (gated)
                    │                                     │
                    └────────────── trial_report ◀─ trial_log / status

treatment_response_register ──▶ root_cause_analysis ◀─ causal pack
        (history as evidence)      (ranked causes, trails, coverage)
                                     │
                                     ▼
                              plan_workup (tests ranked by discrimination
                                     of live causes per cost band)
```

Treatment responses are first-class diagnostic evidence: "antibiotics cleared it and it came back" is a strong signal about *why* it was there. `root_cause_analysis` scores each candidate root cause as `base (strongest causal path) + Σ lab probe signals (matching range status adds, contradicting subtracts) + Σ registered treatment responses`, clamped to [0, 0.99], with the full contribution trail on every score and a coverage ratio — a high score on low coverage says "guess", not "conclusion". `plan_workup` then ranks investigation tests by `Σ (cause score × test resolution) / cost factor`: a cheap test probing the leading hypothesis outranks an expensive one probing dead causes, and causes with no covering test are named as gaps. Suggested hypotheses are registration-ready payloads (`proposal-mode`): the engine proposes, you dispose via `hypothesis_register`.

Everything commits to an **append-only provenance ledger**: every fact carries its source, panel, and timestamps; derived values (Vermeulen free testosterone, HOMA-IR) carry their input fact ids and formula version; `fact_lineage` audits any claim back to measured values.

## Tools (32)

Every tool returns the same envelope shape (`schema_version: 1`): `claims[]` (kind: measured / reported / derived / curated / inferred / conflict / meta, each with evidence ids, optional computation + tier + stale flag), `abstentions[]`, `pack_hashes` (content hashes this output was computed against), and `verifier` (pass/fail, claims checked, sha256 digest).

| Group | Tools |
|---|---|
| Session | `ping` (reports excluded packs + knowledge gaps), `profile_get`, `profile_set` |
| Labs | `lab_normalize` (dry-run), `lab_commit`, `lab_history`, `fact_lineage` |
| Symptoms | `symptom_log` (pack-validated scales: single-item and instrument items — the patient-reported counterpart of lab_commit), `symptom_history` |
| Derived | `derived_free_testosterone` (Vermeulen 1999), `derived_homa_ir` (Matthews 1985) |
| Reasoning | `interpret_panel`, `explore_condition`, `hypothesis_register` (origin: user/engine/agent), `hypothesis_list`, `hypothesis_update` |
| Causal | `root_cause_analysis` (graph scoring: base rates + lab probes + treatment responses, full contribution trails, coverage), `treatment_response_register` (treatment history as diagnostic evidence) |
| Inference | `hypothesis_space` (computed unknowns as creativity scaffolding: untestable causes, uncovered causes, uncommitted probes, unconsumed response patterns, mechanism-family kinship across conditions), `propose_novel_hypothesis` (agent-origin hypotheses, tier D on arrival, refs must resolve before writing, review-required), `literature_probe` (hypothesis-first search over causal nodes, response patterns, cards, and opt-in PubMed) |
| Workup | `plan_workup` (tests ranked by expected discrimination of live causes per cost band; uncovered causes surfaced as gaps) |
| Advocacy | `physician_brief` (specialty routing in evidence order, bilingual request scripts, acquisition routes, legal classes — region-scoped), `region_report` (region coverage + curation queue) |
| Protocols | `propose_protocol` (natural-first ordering; region products attach purchasable rungs with label-honest disclosure — strain/CFU gaps named, stacking ceilings counted), `triage_check` |
| Trials | `trial_begin` (outcomes: lab analytes, derived ids, or single-item symptom scales), `trial_log`, `trial_update_status`, `trial_list`, `trial_report` (lab and symptom deltas independently verified, index-aligned baselines; n=1 caution boilerplate travels with every report) |
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
| `ASCLEPIUS_PATIENT` | `user` | Patient scope for every ledger read/write (multi-patient-ready ledger; single patient today) |
| `ASCLEPIUS_PUBMED` | unset (off) | `1` enables PubMed egress in `literature_verify` — **the claim text is sent to NCBI eutils** |
| `ASCLEPIUS_PUBMED_KEY` | unset | Optional NCBI API key |
| `ASCLEPIUS_BRIDGE` | unset (off) | `1` posts every verified envelope (compact audit view: digest, claims, evidence, numbers) to the bridge log |
| `ASCLEPIUS_BRIDGE_PATH` | `~/.local/share/anticonf/data/bridge.jsonl` | Bridge log location |

Network egress is **off by default**. The only egress is PubMed lookups, behind an explicit flag, with abstracts cached locally in the ledger.

## The external audit ring (L5)

The engine verifies itself (L0–L3, in-process); the audit ring makes the *narration* checkable. Set `ASCLEPIUS_BRIDGE=1` and every verified tool call posts a compact envelope record to the anticonf bridge (JSONL): tool, verifier digest, and every claim's id, kind, statement, evidence refs, and top-level numbers (`value`s, `score`/`base`, counts).

```
bridge: 2 records, 2 envelopes
  ping: 7 claims, sha256:6c958a2ad902e…
  root_cause_analysis: 8 claims, sha256:9db7098d8d1a…

claim c2 : Gut dysbiosis (gut-skin axis): score 0.85 (base 0.35)
           numbers: {base: 0.35, score: 0.85}   evidence: [id:gut_dysbiosis, pack:causal, ledger:treatment_responses/tres_…]
  narration "…gut dysbiosis at 0.85" → PASS — matches claim c2
  narration "…gut dysbiosis at 0.62" → FAIL — prose says 0.62, envelope says 0.85
```

**The audit loop, as a workflow:** an LLM client narrates freely — that is its job — but its numbers and attributions are diffable in one pass against the envelope: every number the narration asserts must match a claim's numbers or cite its evidence; anything unmatched is flagged. Run it as a post-response pass (opencode audit subagent, or `anticonf.bridge_read` + a diff) on any exchange that made a medical assertion. The engine guarantees the envelope told the truth; this ring catches the narrator who didn't.

## Curation


The medical knowledge lives in versioned TOML content packs, not compiled into the binary:

```
content-packs/
├── analytes/              # id, aliases, units, molar mass, plausibility bounds
├── ranges/                # population + functional ranges w/ source, confidence, review_by
├── rules/                 # declarative interpretation rules (analyte, kind, status) → finding
├── conditions/            # root-cause cards: mechanisms, biomarkers, honest tier notes
├── interventions/         # dosing, ceilings, contraindications, interactions, flags
├── triage/                # red-flag patterns → urgency
├── causal/                # typed causal graphs: nodes (root_cause/mechanism/symptom),
│                          #   weighted tiered edges, lab probes per node
├── treatment-responses/   # observable response patterns → score shifts on causal nodes
├── tests/                 # investigation tests: what they discriminate (node + resolution),
│                          #   specimen, cost band, caveats, availability notes
└── regions/               # layered geography: _base < bloc (eu) < country (gr);
                           #   test acquisition routes, legal classes, physician
                           #   routing with bilingual request scripts, and
                           #   purchasable [[product]] entries mapping
                           #   interventions to real regional products
                           #   (same brand elsewhere = separate entry,
                           #   linked by shared_brand — never smeared)
├── symptoms/              # patient-reported outcome scales: single-item 0-10
                           #   and multi-item instruments (adapted wordings
                           #   labeled; validated originals cited, not copied)
```

Region resolution is deepest-layer-wins with per-entry `resolved_from` provenance — a Greek DUTCH route inherits from the EU layer and says so. Region data is never inferred: unset region, unsupported country, or missing route abstains loudly with the curation path named. `region` is private profile data (`profile_set {region: {country: "GR"}}`); legal classes are last-reviewed data, not legal advice.

Everything is data: edit a pack, restart the server, done. Seeds shipped here are approximations marked with confidence levels and `review_by` dates — the intended workflow is that **you** curate them (and commit your curation) until the review queue is empty. `literature_verify` with `ASCLEPIUS_PUBMED=1` fetches real abstracts to check or upgrade any citation.

Range scoping supports sex and age bands. Rules use AND semantics over `(analyte, range_kind, status)` conditions, so they survive range curation without edits.

## Architecture

```
crates/
├── asclepius-core/
│   ├── packs.rs       # L0 load gate: hash, parse, cross-validate packs;
│   │                  #   degraded mode + visible knowledge gaps; DAG check
│   ├── envelope.rs    # assertion envelopes: claim kinds, evidence bindings,
│   │                  #   computation, stale flags, pack-hash pinning
│   ├── verify.rs      # L3 verifier: independent recomputation of every
│   │                  #   claim (formulas reimplemented from publications);
│   │                  #   corruption-detection suite is the merge gate
│   ├── causal.rs      # causal graphs: traversal, weighted scoring,
│   │                  #   lab-probe signals, treatment-response evidence
│   ├── investigate.rs # workup planning: tests ranked by discrimination
│   │                  #   of live causes per cost band; gaps surfaced
│   ├── navigate.rs    # region layers (_base < bloc < country), deepest-
│   │                  #   wins resolution with provenance; routes, legal
│   │                  #   classes, physician routing
│   └── ...            # units, analyte registry, dual ranges, rules,
│                      #   Vermeulen/HOMA-IR, SQLite ledger (append-only,
│                      #   patient-scoped, provenance on every row),
│                      #   PubMed client, bridge
└── asclepius-mcp/    # thin rmcp (stdio) shell exposing the engine;
                        #   every handler emits envelopes through verify
```

Unit-conversion math and formulas are pure functions with unit tests against published worked examples (e.g. Vermeulen free-T ≈ 5.2 ng/dL for TT 500 ng/dL / SHBG 30 nmol/L / albumin 4.3 g/dL). The ledger is single-writer and patient-scoped (`patient_id` on every row, `origin` on hypotheses: user / engine / agent). Known trade-off, documented honestly: write-then-verify handlers (`lab_commit`, `treatment_response_register`) commit before the envelope verifies, so a handler-contract bug can leave a committed row behind a refused response — input validation runs first and the corruption suite covers the contracts, but transactional verify is on the debt list.

## Status

v0.10.0 — the full loop works end to end, every response travels as a verifier-checked assertion envelope, causal reasoning ranks root causes (now for acne, PSSD, and PAWS), `plan_workup` orders the investigation, the advocacy layer routes it regionally, protocols ground to purchasable products, the audit ring posts every envelope, the symptom ledger carries patient-reported outcomes — and **the inference workbench is live**: `hypothesis_space` computes the graph's open unknowns (including mechanism kinship across conditions — the gut-tryptophan axis linking acne dysbiosis at 0.85 to PSSD crash vulnerability), `propose_novel_hypothesis` gives reasoning models a disciplined landing place (origin agent, tier D on arrival, refs must resolve before anything is written, review-required by default +90 days), and `literature_probe` checks an idea against everything the engine knows before it is believed. Creativity in, honesty labeled on arrival. Ledger migrations (v4→v7) run automatically — back up `asclepius.db` before upgrading.

## How I built this

**The problem.** Lab reference ranges tell you whether a number is inside a population. They do not tell you what is causing an out-of-range number, and a single verdict hides the reasoning you would need to trust it.

**Design decisions worth arguing about:**

- **Dual ranges, always.** Population reference ranges *and* functional ranges, side by side, because a value can be "normal" for the population and still be the wrong value for this person. Presenting one without the other is how benign findings get over- or under-called.
- **Root cause before intervention.** The tool walks the causal chain — symptoms, mechanism, contributing factors — and only then proposes anything. The "natural-first" ordering is a deliberate consequence of that ordering, not a separate preference.
- **Provenance on every number.** Each figure that enters an interpretation keeps where it came from, so a downstream claim can be traced to the measurement rather than to the model's memory. This is the same principle as in `anticonf`: no conclusion without an audit trail.
- **Every finding carries a source and a confidence level.** A claim that cannot be sourced is marked as inference, visibly, rather than being smoothed into a confident sentence.
- **Content packs are data, not code.** The clinical knowledge lives in reviewable packs, so the reasoning can be corrected by editing a file rather than patching a binary.

**What I would change with more time:** the causal walk is hand-authored per condition. Encoding the chains as a queryable graph would let the tool traverse them instead of reciting them, and would make gaps in the knowledge base visible rather than invisible.

## License

[MIT](LICENSE) — with the medical disclaimer above, which the license does not waive.
