# ADR-0004 — Mapping device policy onto AGL mutation scopes

- **Status:** Accepted, with an open question for upstream
- **Date:** 2026-09-11

## Context

ADR-0003 made `autogenous` the box's promotion authority. Building the first
real mutation surface — sensor poll interval against thermal headroom —
surfaced a mismatch that is worth recording rather than quietly working around.

`agl_types::MutationScope` has ten variants:

```
PromptContext · RoutingBudget · RetrievalRerank · CacheMemory
AgentTopology · ApplicationCode · SchemaMigration · SecurityPolicy
CompilerIr · Constitutional
```

**None of them describe device or hardware policy.** Every variant is a
software concern. This is consistent with what autogenous has been used for so
far — its shipped examples are security antibody packages — but it means a box
tuning its own sampling rate has no scope that genuinely fits.

The four auto-promotable scopes are `PromptContext`, `RoutingBudget`,
`RetrievalRerank` and `CacheMemory`. Everything above requires governed or
constitutional promotion, i.e. a human.

## Decision

Model a sensor poll interval as **`MutationScope::RoutingBudget`**, at
`Authority::AutoReversible`.

The reasoning: a poll interval allocates a *sampling budget* against a physical
constraint, which is the closest honest reading of "routing budget", and it is
auto-promotable — appropriate for a change that is immediately and completely
reversible by writing one number back to a file.

**This is an interpretation, not a fit**, and the code says so in its module
documentation. The alternative — inventing a local scope enum — was rejected
because it would fork the type that autogenous's admission check reasons about,
which is the whole reason for using autogenous.

## Consequences

- The genome's `capability_ceiling` is `AutoReversible`. This box may adjust its
  own sampling budget and may never grant a descendant more authority than it
  holds — enforced upstream by `Mutation::admissible` and covered by a test
  against the real implementation, not a local copy.
- Any future surface that is *not* immediately reversible — clock frequency,
  firmware, anything touching the bootloader — must not reuse this mapping. It
  needs `Governed` at minimum, which means a human in the loop.
- Every proposal carries a `rollback_target` and an `expires_at` of 24 hours.
  Expiry is a safety property, not bookkeeping: a tuning decision derived from
  an hour-old thermal reading should not still be in force tomorrow.

## Open question for upstream

Does `autogenous` want a device/actuator scope — or is the intended answer that
physical-world mutations are always `Governed` and never auto-promotable?

That is a real design question about the trust model, not a gap to patch
locally. The second answer is defensible: a bad poll interval wastes power,
while a bad clock setting can cook a board, and the type system currently makes
no distinction between them. Worth raising as an issue on `ruvnet/autogenous`
rather than deciding unilaterally here.
