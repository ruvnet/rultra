# ADR-0006 — Repeatable is not the same as correct

- **Status:** Accepted
- **Date:** 2026-09-12
- **Found by:** adding an ultrasonic range finder

## Context

ADR-0002 gave verification three states: `Working`, `AcksButSilent`,
`Untested`. Those covered every device on the board at the time, because each
device either produced correct output, produced none, or had never been tried.

Adding the HC-SR04 range finder produced a case none of them fit.

The part answers: pulsing GPIO23 yields a rising edge on GPIO24. The driver
returns readings. After rejecting trigger crosstalk, those readings are
**stable** — 4.58 cm mean, 0.62 cm standard deviation over eight samples,
tightened from 1.82 cm.

And yet nobody has confirmed there is anything 4.6 cm from the sensor.

`AcksButSilent` is wrong — it produces output. `Untested` is wrong — it has
been exercised at length. `Working` is wrong in the way that matters most,
because a reader who sees "working" concludes the distances are *right*.

## Decision

Add a fourth state:

```rust
/// Produces stable, plausible output, but no reading has been checked
/// against a known reference.
Unvalidated,
```

Ranked between `Working` and `AcksButSilent`: better than a device that emits
nothing, worse than one whose output has been confirmed.

## Why this distinction is the important one

**Repeatability is a property of the measurement path. Accuracy is a property
of its agreement with the world.** A sensor with a constant offset, a wrong
scale factor, or a mis-specified speed of sound returns beautifully repeatable
numbers that are beautifully wrong — and every statistic computed from them
looks healthy. Low variance is evidence that the *path* is stable, and is
routinely mistaken for evidence that the *values* are true.

This is the same failure as ADR-0005, one level down. There, a fitness function
measured the wrong thing while every test passed. Here, a sensor measures
something consistently while nobody has checked what. In both cases the numbers
were fine and the meaning was not.

Most real sensors live in this state permanently, because calibrating against a
known reference takes deliberate effort that shipping does not require. Naming
it makes that visible instead of letting it hide inside "working".

## Consequences

- `Range` is `Unvalidated`, and its evidence records exactly what is and is not
  known, including the variance figures and what would settle it: a measurement
  at a ruler-known separation.
- Promotion to `Working` requires a reading compared against a known distance —
  a physical act by a person, like every other promotion in this project.
- The console shows `unvalidated` in its own colour rather than folding it into
  a working badge.
- A test asserts the ordering, so the two can never be silently conflated.
