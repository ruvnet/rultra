//! `rultra` — one governed self-optimization cycle, end to end.
//!
//! ```text
//! observe parent ─▶ propose ─▶ admit ─▶ apply ─▶ observe child ─▶ score
//!                                                                    │
//!              promote ◀── gate passes ─────────────────────────────┘
//!              roll back ◀── gate refuses
//! ```
//!
//! Every step appends to a signed witness chain, so the answer to "why is this
//! box configured this way?" is a file, not a guess.

mod cycle;

use anyhow::Context as _;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("cycle") {
        "cycle" => {
            let samples = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(12u32);
            let report = cycle::run(samples).context("running a governed cycle")?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "chain" => {
            let path = cycle::chain_path();
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("no witness chain at {}", path.display()))?;
            print!("{text}");
        }
        "policy" => {
            let a = rultra_evolve::policy::Applier::new(cycle::policy_path());
            println!("{}", serde_json::to_string_pretty(&a.load())?);
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: rultra cycle [samples] | chain | policy");
            std::process::exit(2);
        }
    }
    Ok(())
}
