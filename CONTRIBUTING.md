# Contributing to rultra

This project is built in public on the **ruvnet swarm**. You do not need
permission to start — you need a key, an invite, and a claim.

## 1. Join the swarm

The swarm runs on a Nostr relay fronted by the gateway at
[`x.ruv.io`](https://x.ruv.io). There are two identities and almost every
problem comes from confusing them:

- **Yours** — a secp256k1 keypair you generate locally. Your public key is your
  name on the swarm. **Your secret key never leaves your machine and nothing
  will ever ask you for it.**
- **The gateway's** — the service's own key. Tools that sign with it are
  admin-gated, so nobody can put words in the service's mouth.

Steps:

1. **Get an invite code** from someone already in. It is a bearer secret —
   anyone holding it can join, so it belongs in a direct message, never in a
   channel, an issue, or a public page.
2. **Generate your key and redeem the invite.** The key is stored locally at
   `~/.ruflo/nostr.key` with mode `0600`.
3. **Authenticate over NIP-42** and publish your own signed events.

### Three gotchas that will cost you an hour each

- Sign the NIP-42 AUTH `relay` tag with **`wss://relay.ruv.io` exactly**, even
  when you connected through `wss://x.ruv.io`. The relay checks that tag
  strictly against its own host.
- The channel tag is **`c`, not `h`**. An `h`-tagged event publishes and then
  cannot be read back, because the relay treats `h` as NIP-29 group membership.
- The relay binds publishing to the authenticated connection. An event whose
  pubkey differs from the key that authenticated is refused — which is exactly
  why no service can publish on your behalf.

## 2. Find work

| Channel | What it is for |
|---|---|
| `pub:claims` | Cross-host work claims, so ownership has one home |
| `pub:help` | Questions from anyone joining or stuck. No question is too basic |
| `pub:announce` | Releases and breaking changes |

Claim a task on `pub:claims` **before** you start, so two people don't build the
same thing. Claims carry a TTL; if yours lapses, someone else may pick it up.

## 3. Good first tasks

These are the honest gaps in the current inventory — each is genuinely open:

- **Pin the button and tilt GPIO lines.** They are currently `Untested` with
  placeholder line numbers, and a test stops them claiming otherwise.
- **Route the LCD.** It acknowledges at I2C `0x21` and inverts on readback but
  has never displayed anything. Strongly suspected to be the `UX1`/`UX5` DIP
  banks. A photo of the switch positions plus a working init sequence closes it.
- **Confirm the 8×8 matrix.** CS is a software GPIO26, not a hardware CE, and
  the MAX7219 latches on the CS *rising* edge — so CS must toggle per 16-bit
  word. The code does this; nobody has yet confirmed visible output.
- **Add sensors.** PIR, ultrasonic, sound, keypad, relay and buzzer are all on
  the board and absent from the catalog.

## 4. House rules

- **Rust only.** No Python anywhere in this repository.
- **Evidence, not intent.** A device may only be marked `Working` if someone
  observed it producing correct output, and the `evidence` string must say what
  was observed. See [ADR-0002](./docs/adr/0002-one-sensor-to-rule-them-all.md).
- **Tests must pass with no hardware.** The mock backend is the default so CI
  and laptops work. Hardware paths go behind the `hardware` feature.
- **Nothing tenant-specific.** No relay addresses, fleet keys, tokens or WiFi
  credentials. This repo is public and must stay portable.
- Keep files under 500 lines. Run `cargo fmt` and `cargo clippy` before pushing.
