# ADR-0001 — What rultra is, and what it refuses to be

- **Status:** Accepted
- **Date:** 2026-09-11

## Context

A Raspberry Pi 5 on an Elecrow CrowPi V3.2 carries roughly twenty sensors and
actuators across I2C, SPI, GPIO, 1-Wire and sysfs. The usual outcome is a drawer
of one-off scripts: each works once, none compose, and none can say how much
they can be trusted.

Separately, the ruvnet stack already contains the pieces of a self-improving
system — `ruvector` (vector memory, RVF containers), MetaHarness (benchmarking
and evolution), `autogenous` (governed evolutionary promotion), `ruflo`
(orchestration). What has not existed is a **physical box** where these close a
loop against real measurements rather than synthetic ones.

## Decision

`rultra` is a self-optimizing edge node with two halves:

1. **One sensing surface.** Every device behind a single trait, with an explicit
   record of how well each is known to work (ADR-0002).
2. **A governed improvement loop.** The box measures itself, proposes changes to
   its own configuration, and promotes them only through a hard fitness gate
   with canary and rollback (ADR-0003).

## What this is not

- **Not a magic self-rewriting repository.** Mutations are typed, scoped,
  expiring and reversible. Anything outside that envelope is a human's job.
- **Not a hardware abstraction layer for every Pi board.** It targets one board
  precisely, because honest verification requires a specific physical device.
- **Not tenant-coupled.** No relay addresses, fleet keys, API tokens or network
  credentials belong in this repository. It is public and must stay portable.

## Consequences

- A claim of "works" in this repo must trace to an observation. Where nobody has
  looked, the code says so — see ADR-0002.
- The project is useful at step one: `rultra-sense` is valuable as a plain
  telemetry tool before any evolution exists.
- Targeting one board means porting is a real cost. Accepted: a trustworthy
  inventory of one board beats an untrustworthy inventory of ten.
