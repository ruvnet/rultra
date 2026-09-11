# ADR-0002 — One sensing surface, with verification in the type system

- **Status:** Accepted
- **Date:** 2026-09-11

## Context

Hardware inventories rot, and they rot in a specific way: they record **intent**
("the board has an LCD") rather than **evidence** ("the LCD acknowledges on I2C
0x21 but no configuration has ever produced visible output").

This project hit that exact case. During bring-up:

- The BH1750 light sensor read 58.3 lux — genuinely working.
- The HT16K33 segment display showed changing digits — genuinely working.
- The PCF8574 LCD **acknowledges at 0x21 and inverts on readback**, yet has
  never displayed anything. Its signal is believed to be routed away by the
  CrowPi's `UX1`/`UX5` DIP banks.
- The MAX7219 matrix sits on SPI behind a **software chip-select on GPIO26**,
  not a hardware CE line. SPI has no acknowledgement, so a present `spidev`
  node proves the bus exists and nothing about the chip.

A boolean `present: bool` cannot express any of that, and an inventory that
flattens these three cases into one is actively misleading.

## Decision

Model verification as a type, not a comment:

```rust
pub enum Verification {
    Working,        // observed producing correct output
    AcksButSilent,  // answers on the bus, no observable effect yet
    Untested,       // documented, never exercised here
}
```

`Working` is the only variant that licenses a claim of functionality in docs,
a README, or a status endpoint.

Two separate notions are kept apart deliberately:

- **`CATALOG`** — what the board is documented to carry, with the verification
  state and the evidence behind it.
- **`Backend::probe()`** — what physically answered *just now*.

`probe` never upgrades a catalog verification. A device that responds is
reported as responding **at its existing verification level**, enforced by test
(`probe_preserves_catalog_verification`).

## Consequences

- An unverified device cannot quietly read as a working one. This is the whole
  point.
- `Working` entries must carry substantive evidence; a test enforces it.
- Advertising fewer working devices than the marketing copy is the intended
  outcome, not a defect.
- Two backends: `mock` (default, so CI and laptops work with no hardware) and
  `linux` (behind the `hardware` feature). Tests run everywhere.
