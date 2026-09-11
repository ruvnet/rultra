# ADR-0003 — Composing autogenous, ruvector, MetaHarness and ruflo

- **Status:** Accepted
- **Date:** 2026-09-11

## Context

The premise of this box is a loop: observe → score → propose a change → gate it →
promote or roll back. Four ruvnet components plausibly claim a piece of that
loop, and the narrative overlap between them is much larger than the
implementation overlap. Building on the narrative would have produced a design
that could not be written in Rust and could not run offline.

A grounded survey of the actual repositories settled it.

## What is actually true

| Component | Language | On-device? | Real role here |
|---|---|---|---|
| [`autogenous`](https://github.com/ruvnet/autogenous) | **Rust**, 15-crate workspace | **Yes** | Governed promotion state machine |
| [`ruvector`](https://github.com/ruvnet/ruvector) (`crates/rvf/*`) | **Rust** | **Yes** | Telemetry memory, witness chain |
| [`metaharness`](https://github.com/ruvnet/metaharness) | TypeScript | **No** | Scoring *methodology*, offline |
| [`ruflo`](https://github.com/ruvnet/ruflo) | TypeScript | No (subprocess/HTTP) | Operator orchestration, off hot path |

Three findings drove the decision:

1. **autogenous is Rust and already does the hard part.** `agl-types` defines
   `Genome`, `Mutation`, an ordered `Authority` ceiling and an 8-field
   `FitnessVector`; `FitnessVector::passes_hard_gates` (`crates/agl-types/src/lib.rs:162`)
   implements `min`-semantics so a strong score in one dimension can never
   offset a safety or governance failure. `promotion` does canary 1→10→50→100%,
   `deployment` performs a two-phase *verified* rollback emitting a signed
   receipt, and `envelope` requires two independently signed evaluations, making
   an empty promotion structurally impossible. **We do not rebuild any of this.**

2. **MetaHarness is TypeScript, and its loop assumes a sandbox, a git tree and a
   benchmark suite it mutates and re-runs.** That is a CI/dev-machine workflow,
   not something that runs continuously on a Pi in the field. Its ADR-076
   methodology — five gates plus statistical promotion requiring the child to
   beat the parent with a lower-95% bootstrap CI above zero — is excellent and
   portable *as a specification*. The code is not embeddable in a Rust binary.

3. **ruflo is TypeScript with only a subprocess or HTTP boundary into Rust.**

## Decision

**1. autogenous is the single promotion authority.** One box, one gate. MetaHarness's
`@metaharness/flywheel` is also a signed-receipt promotion engine, but adopting
two would mean two audit trails and two definitions of "promoted". autogenous
wins on the constraint that matters: it is Rust and it runs on-device.

**2. MetaHarness is an offline tool, and its scoring rules are re-implemented in
Rust.** We port the *methodology* (five gates, bootstrap CI, parent-vs-child
delta) into a `rultra-score` crate that emits an `agl_types::FitnessVector`. We
do not shell out to `npx` on the hot path. ruvector already uses MetaHarness
exactly this way — offline, to tune its own HNSW parameters against public
benchmarks (`ruvector/docs/metaharness-implementation-plan.md`).

**3. ruvector's `rvf` crates are the telemetry substrate**, linked natively as
Rust. `rvf-runtime` for the store; the cognitive-container witness chain for
provenance.

**4. The loop must run with ruflo down.** ruflo is the operator surface —
remote memory search, swarm coordination, dashboards. Nothing in
observe→score→gate→promote may depend on it. An edge box that stops improving
because a Node process is unreachable is not self-optimizing.

**5. First mutation surface: thermal headroom versus sensor poll rate.** It is
measurable on this board today — `get_throttled` has already returned
`0x80000` (bit 19: a soft temperature limit *has* occurred), and idle die
temperature sits near 57 °C. ruvector's `crates/ruos-thermal` ("Pi 5 thermal
supervisor + over/underclock control", ADR-174) is the actuator, already
written. One metric all the way through a real gate is worth more than five
abstract ones.

## The seams we have to build

Nothing in the stack provides these; they are this repository's actual work:

1. **The sensor abstraction.** No component defines a `Sensor` trait or any
   board driver. Built here — see ADR-0002.
2. **Telemetry → `Mutation`.** autogenous *admits and scores* a typed mutation;
   nothing generates one from sensor drift. Its `generator` crate exists but is
   scoped to synthesising security antibodies from attack evidence.
3. **A Rust scorer producing a `FitnessVector`.** Per decision 2.
4. **One witness chain.** ruvector and autogenous each sign ed25519
   content-addressed receipts, but they are *separate chains*, not
   cross-referenced. A single audit trail spanning "observed X → proposed Y →
   promoted/rolled back" is integration work.
5. **A config applier.** `promotion`/`deployment` model canary and rollback of an
   abstract content-addressed artifact with a health check. Nothing knows what
   "50% canary" means for a poll interval or a clock setting on a Pi.

## Risks

- **autogenous's generic types are unproven outside the security domain.** Every
  shipped example is a security antibody package. `Genome`, `Mutation` and
  `FitnessVector` carry no security-specific fields, so repurposing them for
  device policy looks sound — but it is *unproven*, not merely unbuilt. If the
  types resist, that is a finding worth reporting upstream, not working around
  silently.
- **"Self-optimizing" is a weaker claim than it sounds.** Only autogenous's
  state machine is honestly on-device-capable. MetaHarness-style evolution is a
  periodic offline "benchmark, then ship a signed genome" pattern. The README
  must not imply the box evolves itself in the field beyond what the gate
  actually governs.
