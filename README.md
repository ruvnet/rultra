# rultra — a self-optimizing Raspberry Pi 5 sensor box, in Rust

**One unified sensing surface for every sensor on the board, and a governed loop
that lets the box improve its own configuration without anyone trusting it
blindly.**

Built on [Raspberry Pi 5](https://www.raspberrypi.com/products/raspberry-pi-5/)
+ [Elecrow CrowPi V3.2](https://www.elecrow.com/), in pure Rust, composing the
[ruvnet](https://github.com/ruvnet) stack: `ruvector` for vector memory,
MetaHarness for benchmarking, [`autogenous`](https://github.com/ruvnet/autogenous)
for governed evolutionary promotion.

```bash
rultra-sense inventory   # what the board carries, and how well each part is known
rultra-sense probe       # what answers right now
rultra-sense stream      # JSON-lines telemetry from every sensor
```

---

## The idea most hardware projects skip

Ask a typical Raspberry Pi sensor library whether a device works and you get a
boolean. That boolean is a lie of omission, because there are at least three
genuinely different states, and this board has one of each:

| Device | Bus | State | What is actually known |
|---|---|---|---|
| BH1750 light sensor | I2C `0x5c` | **Working** | Returns 37–58 lux, tracks real light changes |
| HT16K33 segment display | I2C `0x70` | **Working** | Digits observed changing on the physical display |
| PCF8574 LCD | I2C `0x21` | **Acks but silent** | Chip acknowledges and inverts on readback — but has never displayed anything |
| MAX7219 8×8 matrix | SPI, CS on **GPIO26** | **Untested** | SPI has no ACK; a present `spidev` node proves nothing about the chip |

So `rultra` puts that distinction in the type system:

```rust
pub enum Verification {
    Working,        // observed producing correct output
    AcksButSilent,  // answers on the bus, no observable effect yet
    Untested,       // documented, never exercised here
}
```

`Working` is the only variant that licenses the word "works" — in the code, in
this README, or in a status endpoint. A probe reports what responded *now*; it
never upgrades a device's verification level, and a test enforces that.

**This README therefore advertises fewer working devices than the box has.**
That is the intended behaviour.

## Quick start

```bash
git clone https://github.com/ruvnet/rultra && cd rultra
cargo test                      # runs anywhere — mock backend, no hardware needed
./scripts/build-pi.sh           # cross-compile to aarch64 for the Pi
scp target/aarch64-unknown-linux-gnu/release/rultra-sense pi@raspberrypi:/tmp/
```

On the Pi:

```bash
sudo install -m0755 /tmp/rultra-sense /usr/local/bin/
rultra-sense probe
```

```json
{"device":"light","responding":true,"verification":"working","detail":"ACK at 0x5c"}
{"device":"matrix","responding":true,"verification":"untested","detail":"/dev/spidev0.0 present (SPI cannot confirm a peer)"}
{"device":"lcd","responding":true,"verification":"acks_but_silent","detail":"ACK at 0x21"}
```

## Two traps this repo documents so you don't rediscover them

**1. `rvf` on crates.io is not ruvnet's `rvf`.** The published crate of that name
is an unrelated ValueFlows economic vocabulary. ruvnet's RVF must be a git
dependency. Taking the version dependency silently builds the wrong thing.

**2. `RUSTFLAGS` in the environment overrides `.cargo/config.toml`.** It does not
merge. A host-wide `-C link-arg=-fuse-ld=mold` leaks into an aarch64 cross-link
and fails as `collect2: fatal error: cannot find 'ld'` — a message that sends you
hunting for a missing linker that is in fact installed. `scripts/build-pi.sh`
clears it; that is the only fix that survives any host shell config.

## Architecture

Decisions live in [`docs/adr/`](./docs/adr):

- [ADR-0001](./docs/adr/0001-purpose-and-scope.md) — what rultra is, and what it refuses to be
- [ADR-0002](./docs/adr/0002-one-sensor-to-rule-them-all.md) — one sensing surface, verification in the type system
- [ADR-0003](./docs/adr/0003-composition-of-the-ruvnet-stack.md) — composing autogenous, ruvector, MetaHarness and ruflo

## Contributing

Work is coordinated in public on the ruvnet swarm — see
[CONTRIBUTING.md](./CONTRIBUTING.md) for how to join, claim a task, and publish
your progress.

Good first issues are the honest gaps above: the LCD's DIP routing, and
confirming the matrix.

## License

MIT
