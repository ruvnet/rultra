# ADR-0005 — Fitness must encode sustainability, not raw throughput

- **Status:** Accepted
- **Date:** 2026-09-11
- **Found by:** running the loop on real hardware at 83.6 °C

## Context

ADR-0003 chose thermal headroom versus sensor poll rate as the first mutation
surface. The obvious objective for a sensor box is **successful reads per
second**, so that is what the first implementation scored.

Running a real cycle under thermal load exposed the flaw. The box reached
83.6 °C — well past the 75 °C ceiling — and behaved like this:

```
observed   die_temp_c 83.645
proposed   poll-2000  (1000ms -> 2000ms, multiplicative backoff)
observed   die_temp_c 83.590
gated      delta_ci=[-0.500,-0.500] beats_parent=false gates=false safety=0.00
rolled_back
```

Every mechanism worked correctly. The proposal was right, the rollback was
executed and verified, the AND-gate refused on `safety = 0.00`, and the witness
chain recorded all five steps and verified.

**But the refusal was structural, not circumstantial.** Quality was raw
reads/sec, and halving the poll rate necessarily halves reads/sec. A thermal
backoff can therefore *never* produce a positive delta against its parent. The
single safety action this controller exists to take was permanently
unpromotable. The gate was doing its job; the fitness function was wrong.

This is the failure mode a fitness function is most prone to: it was not
mismeasured, it measured the wrong thing, and only a physical run surfaced it.

## Decision

Quality is **sustainable** throughput: successful reads per second, scored as
**zero** whenever the die is above its thermal ceiling.

This encodes a physical truth rather than a policy preference. Readings taken
past the thermal ceiling are about to stop regardless of what the policy wants,
because the firmware throttles on its own — this board has already recorded
`get_throttled` bit 19. Counting them as throughput measures a rate the box
cannot sustain.

With this metric, a backoff that actually brings the die under the ceiling
turns a zero into a positive number and becomes promotable.

## Consequences

- **The controller can only help when sampling is the heat source.** In the run
  above the heat came from four busy cores, so backing off sampling would not
  have cooled anything — and the box still correctly refused the change. That
  is the right behaviour, not a limitation to fix: a governed system should
  refuse a change that does not demonstrably help, even a well-intentioned one.
- **A hot box scores zero on quality *and* zero on safety.** These are not
  redundant. Safety is a hard gate that cannot be bought off; quality is a
  ranking signal among candidates that already passed. Conflating them would
  let a sufficiently large quality gain compensate for a safety miss, which is
  exactly what `min`-semantics exists to prevent.
- **Any future mutation surface needs this check applied deliberately.** The
  question to ask of an objective is not "is it measured correctly?" but "can
  the action I most want the system to take ever score well on it?" A surface
  whose safest action is unpromotable is misdesigned, and it will pass every
  unit test.

## Note

This was found because the loop ran on physical hardware under real thermal
load. A mock backend reports whatever temperature it is told to, and every test
in this repository passed both before and after the fix. That is worth
recording: the tests were not inadequate, they were answering a different
question than the one that mattered.
