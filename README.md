<div align="center">

# rultra

### A self-optimizing Raspberry Pi 5 sensor box, in Rust

**Every sensor on the board behind one trait — and an honest record of how well each one actually works.**

[![CI](https://github.com/ruvnet/rultra/actions/workflows/ci.yml/badge.svg)](https://github.com/ruvnet/rultra/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)
[![rust](https://img.shields.io/badge/rust-1.74%2B-orange)](#build)
[![tests](https://img.shields.io/badge/tests-46%20passing-brightgreen)](#build)
[![platform](https://img.shields.io/badge/target-aarch64%20%C2%B7%20Pi%205-c51a4a)](#build)

[Quick start](#quick-start) · [The idea](#the-idea-most-sensor-libraries-skip) · [Architecture](#the-governed-loop) · [ADRs](./docs/adr) · [Contributing](./CONTRIBUTING.md)

</div>

---

`rultra` runs on a [Raspberry Pi 5](https://www.raspberrypi.com/products/raspberry-pi-5/)
mounted on an [Elecrow CrowPi V3.2](https://www.elecrow.com/) learning board. It does two things:

1. **Unifies the hardware.** Light sensor, LED matrix, LCD, segment display, GPIO —
   one `Sensor`/`Actuator` trait, one JSON-Lines telemetry stream, one command.
2. **Improves its own configuration, under governance.** The box measures itself,
   proposes a change, and promotes it only through a hard fitness gate with
   canary and verified rollback — every step recorded in a signed audit trail.

Built on the [ruvnet](https://github.com/ruvnet) stack: [`autogenous`](https://github.com/ruvnet/autogenous)
for governed evolutionary promotion, [`ruvector`](https://github.com/ruvnet/ruvector)
for vector memory, and MetaHarness's scoring methodology, ported to Rust.

---

## The idea most sensor libraries skip

Ask a typical Raspberry Pi library whether a device works and you get a boolean.
That boolean is a lie of omission — and this one board has a live example of
each of three genuinely different states:

| Device | Bus | State | What is actually known |
|---|---|---|---|
| BH1750 light sensor | I²C `0x5c` | ✅ **Working** | Tracks real light: 58 lux in a lit room, 5.8 in the dark |
| MAX7219 8×8 matrix | SPI0 **CE1** | ✅ **Working** | Lit and confirmed by an observer |
| HT16K33 segment display | I²C `0x70` | ✅ **Working** | Digits observed changing |
| MCP23008 LCD | I²C `0x21` | ⚠️ **Acks but silent** | Chip acknowledges; nothing displayed yet |
| Buttons / tilt | GPIO | ❓ **Untested** | Lines read once, but the BCM numbers were never recorded |

So verification is a **type**, not a comment:

```rust
pub enum Verification {
    Working,        // observed producing correct output
    AcksButSilent,  // answers on the bus, no observable effect yet
    Untested,       // documented, never exercised here
}
```

`Working` is the only variant that licenses the word "works" — in the code, in
this README, or in a status endpoint. `probe()` reports what responded *now* and
**never upgrades** a device's verification level. Tests enforce it: a `Working`
claim must carry substantive evidence, and no device may claim to work while
relying on a placeholder GPIO line.

> **This README therefore advertises fewer working devices than the board has.**
> That is the intended behaviour, not an oversight.

The rule earned its keep three times during bring-up — it caught a device
claiming to work on a pin number nobody had written down, and twice stopped a
display being marked working because a command exited zero.

## Quick start

```bash
git clone https://github.com/ruvnet/rultra && cd rultra
cargo test                    # runs anywhere — mock backend, no hardware required
./scripts/build-pi.sh         # cross-compile to aarch64
scp target/aarch64-unknown-linux-gnu/release/rultra-sense pi@raspberrypi:/tmp/
```

On the Pi:

```bash
sudo install -m0755 /tmp/rultra-sense /usr/local/bin/
rultra-sense inventory        # the catalog, with evidence
sudo rultra-sense probe       # what answers right now
sudo rultra-sense stream      # JSON-Lines telemetry
```

```console
$ sudo rultra-sense probe
{"device":"light","responding":true,"verification":"working","detail":"ACK at 0x5c"}
{"device":"matrix","responding":true,"verification":"working","detail":"/dev/spidev0.1 present (SPI cannot confirm a peer)"}
{"device":"lcd","responding":true,"verification":"acks_but_silent","detail":"ACK at 0x21"}

$ sudo rultra-sense stream 1000
{"device":"light","at":1789166455,"value":{"kind":"scalar","n":37.5,"unit":"lux"},"verification":"working"}
{"device":"cpu_temp","at":1789166455,"value":{"kind":"scalar","n":57.3,"unit":"celsius"},"verification":"working"}
```

Drive the outputs:

```bash
sudo rultra-sense matrix heart
sudo rultra-sense matrix scroll "58 LUX 57C"
sudo rultra-sense lcd write "rultra online" "matrix: OK"
```

## The governed loop

```
   observe  ──▶  score  ──▶  gate  ──▶  promote  ──▶  (or roll back)
      │            │          │           │                │
      └────────────┴──────────┴───────────┴────────────────┘
                       one signed witness chain
```

[`autogenous`](https://github.com/ruvnet/autogenous) owns the **gate**: typed
mutations, an authority ceiling no descendant may exceed, and an AND-gate with
`min` semantics — so a strong score in one dimension can *never* offset a safety
or governance failure. Promotion additionally requires that a rollback was
actually executed and verified, not merely available.

`rultra` owns the translation on either side: turning sensor readings into a
typed mutation, and turning a promoted mutation into a change on a real box.

| Crate | Responsibility | Tests |
|---|---|---|
| [`rultra-sense`](./crates/rultra-sense) | One trait over every device, with verification provenance | 15 |
| [`rultra-score`](./crates/rultra-score) | Parent-vs-child scoring → an `agl-types` `FitnessVector` | 7 |
| [`rultra-evolve`](./crates/rultra-evolve) | Telemetry → typed mutation; applier with verified rollback | 13 |
| [`rultra-witness`](./crates/rultra-witness) | One signed, hash-chained audit trail across the loop | 11 |
| [`rultra`](./crates/rultra) | The binary: runs one governed cycle against real hardware | — |

```console
$ sudo rultra cycle          # one full observe → score → gate → promote cycle
$ sudo rultra chain          # the signed causal record
$ sudo rultra policy         # what is in force right now
```

**The first mutation surface is thermal headroom versus sensor poll rate** —
chosen because it is measurable today: this board's `get_throttled` has already
returned `0x80000` (bit 19, a soft temperature limit *has* occurred). The
controller backs off multiplicatively when hot and advances additively when
cool: fast to retreat, slow to advance, so the box settles rather than
oscillates. A window under 10 samples produces no proposal at all.

## Two traps, documented so you don't lose an evening

**1. Your SPI device is silently running at 125 MHz.**

`spidev` falls back to the devicetree `spi-max-frequency` whenever a speed is
never set explicitly. On a Raspberry Pi that is `125000000` — **12.5× the
MAX7219's 10 MHz ceiling**. It fails *silently and identically* on every
chip-select, so sweeping CE0/CE1 looks like a wiring fault and sends you hunting
the wrong thing entirely. Always set the speed per transfer.

**2. `RUSTFLAGS` in the environment overrides `.cargo/config.toml` — it does not merge.**

A host-wide `-C link-arg=-fuse-ld=mold` leaks into an aarch64 cross-link and
fails as `collect2: fatal error: cannot find 'ld'` — a message that sends you
hunting for a missing linker that is, in fact, installed. `scripts/build-pi.sh`
clears it; that is the only fix that survives any host shell configuration.

**Bonus, CrowPi-specific.** The vendor manual transposes the BCM numbers between
its two SPI rows: the matrix is on *physical pin 26* = BCM **GPIO7** = **CE1**
(`/dev/spidev0.1`), not GPIO26. BCM GPIO26 is a button line. And the LCD is an
**MCP23008** expander, not the common PCF8574 backpack — PCF8574-style writes
land in its `IODIR`/`GPIO` registers, which is exactly why it acknowledges and
appears to invert on readback while displaying nothing.

## Build

Requires Rust 1.74+. Tests use a **mock backend by default**, so CI and
development machines need no hardware — if a test ever needs a real device to
pass, the abstraction has leaked.

```bash
cargo test                                   # mock backend
cargo build --features rultra-sense/hardware # real I²C/SPI/GPIO, Linux only
./scripts/build-pi.sh                        # aarch64 cross-build
```

CI runs `cargo fmt --check`, `clippy -D warnings`, the test suite with no
hardware, an aarch64 cross-build, and `cargo audit`.

## What a real run looks like

Heated to 83.6 °C with four busy cores, the box proposed backing off, applied
it, measured, and the gate refused:

```console
$ sudo rultra cycle 10
{
  "decision": "rolled_back",
  "from": { "poll_interval_ms": 1000 },
  "to":   { "poll_interval_ms": 2000 },
  "reason": "delta_ci=[-0.500,-0.500] beats_parent=false gates=false safety=0.00 rollback_verified=true",
  "witness_entries": 5,
  "witness_verified": true
}
```

That run found a real flaw in the design — the objective made the controller's
own safety action structurally unpromotable, and every unit test passed both
before and after the fix. [ADR-0005](./docs/adr/0005-fitness-must-encode-sustainability.md)
is the writeup. It is the clearest argument in this repo for running a control
loop on physical hardware rather than a simulation.

The witness chain is continuous across separate invocations, so the record
survives restarts:

```console
seq  0  observed    {"die_temp_c": 83.645, "read_error_rate": 0.0, "samples": 10}
seq  1  proposed    {"mutation_id": "poll-2000-...", "parent_genome_hash": "371e17b2..."}
seq  2  observed    {"die_temp_c": 83.590, ...}
seq  3  gated       {"passed": false, "reason": "delta_ci=[-0.500,-0.500] ..."}
seq  4  rolled_back {"restored_hash": "371e17b2...", "verified": true}
```

## Honest status

This is a working research prototype, not a product.

- **"Self-optimizing" is a weaker claim than it sounds.** Only autogenous's
  state machine genuinely runs on-device. MetaHarness-style evolution assumes a
  sandbox and a git tree — a CI workflow, not something running in a field.
- **autogenous's generic types are unproven outside the security domain.** Every
  shipped upstream example is a security antibody package. Repurposing them for
  device policy looks sound but is unproven — see [ADR-0004](./docs/adr/0004-mutation-scope-mismatch.md).
- **The witness chain does not detect tail truncation** without an external
  anchor, and offers no defence against an attacker holding the signing key.
  Both limits are stated in the module docs rather than implied away.

## Architecture decisions

- [ADR-0001](./docs/adr/0001-purpose-and-scope.md) — what rultra is, and what it refuses to be
- [ADR-0002](./docs/adr/0002-one-sensor-to-rule-them-all.md) — one sensing surface, verification in the type system
- [ADR-0003](./docs/adr/0003-composition-of-the-ruvnet-stack.md) — composing autogenous, ruvector, MetaHarness and ruflo
- [ADR-0004](./docs/adr/0004-mutation-scope-mismatch.md) — mapping device policy onto AGL mutation scopes
- [ADR-0005](./docs/adr/0005-fitness-must-encode-sustainability.md) — fitness must encode sustainability, not raw throughput

## Contributing

Built in public on the ruvnet swarm. [CONTRIBUTING.md](./CONTRIBUTING.md) has
the full on-ramp — generating your key, claiming work on `pub:claims`, and the
three protocol gotchas that each cost an hour.

Good first tasks are the honest gaps above:

- **Pin the button and tilt GPIO lines** — currently `Untested` with placeholder
  numbers, and a test stops them claiming otherwise.
- **Get the LCD displaying** — the driver is written; it has not yet been seen working.
- **Add sensors** — PIR, ultrasonic, sound, keypad, relay and buzzer are all on
  the board and absent from the catalog.

House rules: **Rust only**, evidence over intent, and tests must pass with no
hardware attached.

## License

MIT © [ruvnet](https://github.com/ruvnet)
