//! One governed cycle against real hardware.

use agl_types::HardGates;
use ed25519_dalek::SigningKey;
use rultra_evolve::{genome_for, policy::Applier, propose, Observation, SensePolicy};
use rultra_score::{score, Measurement};
use rultra_sense::{Backend, DeviceId, Value};
use rultra_witness::{Chain, Event};
use serde_json::json;

/// State directory. `RULTRA_STATE_DIR` overrides it, so the cycle can be run
/// and tested without root.
pub fn state_dir() -> std::path::PathBuf {
    std::env::var("RULTRA_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/rultra"))
}

/// Where the active policy lives.
pub fn policy_path() -> std::path::PathBuf {
    state_dir().join("policy.json")
}

/// Where the witness chain is appended.
pub fn chain_path() -> std::path::PathBuf {
    state_dir().join("witness.jsonl")
}

/// The signing key.
///
/// Read from disk, generated on first run with 0600 permissions. A key that
/// lives only in memory would make every restart a new identity, and a chain
/// nobody can verify across reboots is not an audit trail.
fn signing_key() -> anyhow::Result<SigningKey> {
    let path = state_dir().join("witness.key");
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            let mut k = [0u8; 32];
            k.copy_from_slice(&bytes);
            return Ok(SigningKey::from_bytes(&k));
        }
    }
    // Derive from OS entropy without pulling in a rand version dependency.
    //
    // read_exact, never fs::read: /dev/urandom is an infinite stream, so
    // reading "the whole file" never returns.
    let mut seed = [0u8; 32];
    {
        use std::io::Read as _;
        let mut f = std::fs::File::open("/dev/urandom")?;
        f.read_exact(&mut seed)?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, seed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(SigningKey::from_bytes(&seed))
}

/// The cycle deliberately uses an UNCACHED backend.
///
/// A measurement window has to sample the world; sampling a cache would make
/// the parent and child observations partly the same readings, and a comparison
/// against yourself always looks stable. The console caches because it is
/// displaying; this is measuring, and those want opposite things.
fn backend() -> Box<dyn Backend> {
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    {
        if let Ok(b) = rultra_sense::backend::linux::LinuxBackend::open() {
            return Box::new(b);
        }
        eprintln!("hardware backend unavailable; using mock");
    }
    Box::new(rultra_sense::backend::mock::MockBackend::crowpi())
}

fn scalar(v: &Value) -> Option<f64> {
    match v {
        Value::Scalar { n, .. } => Some(*n),
        _ => None,
    }
}

/// Sample the box for one window under a given policy.
///
/// Quality is **sustainable** successful reads per second: throughput, scored
/// as zero whenever the die is above its thermal ceiling.
///
/// The naive metric — raw reads per second — was measured on hardware at
/// 83.6 C and produced a structural bug. Halving the sample rate necessarily
/// halves reads/sec, so a thermal backoff could never beat its parent on
/// quality, and the one safety action the controller exists to take was
/// permanently unpromotable. The gate correctly refused it every time.
///
/// Scoring an overheating box at zero throughput encodes the physical truth:
/// readings taken past the thermal ceiling are about to stop, because the
/// firmware will throttle regardless of what this policy wants. See ADR-0005.
fn observe(b: &mut dyn Backend, policy: &SensePolicy, samples: u32) -> (Measurement, Observation) {
    let mut temps = Vec::new();
    let mut quality = Vec::new();
    let mut errors = 0u32;
    let mut latencies = Vec::new();

    for _ in 0..samples {
        let t0 = std::time::Instant::now();
        let ok = match b.read(DeviceId::Light) {
            Ok(r) => scalar(&r.value).is_some(),
            Err(_) => false,
        };
        latencies.push(t0.elapsed().as_secs_f64() * 1000.0);
        if !ok {
            errors += 1;
        }
        if let Ok(r) = b.read(DeviceId::CpuTemp) {
            if let Some(t) = scalar(&r.value) {
                temps.push(t);
            }
        }
        // Sustainable successful reads per second at this poll interval.
        let hz = 1000.0 / policy.poll_interval_ms as f64;
        let sustainable = temps
            .last()
            .map(|t| *t < rultra_evolve::policy::THERMAL_CEILING_C)
            .unwrap_or(true);
        quality.push(if ok && sustainable { hz } else { 0.0 });
        std::thread::sleep(std::time::Duration::from_millis(
            policy.poll_interval_ms.min(250),
        ));
    }

    let mean_temp = if temps.is_empty() {
        0.0
    } else {
        temps.iter().sum::<f64>() / temps.len() as f64
    };
    let error_rate = errors as f64 / samples.max(1) as f64;
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p99 = latencies
        .get((latencies.len() as f64 * 0.99) as usize)
        .or_else(|| latencies.last())
        .copied()
        .unwrap_or(0.0);

    let obs = Observation {
        die_temp_c: mean_temp,
        read_error_rate: error_rate,
        samples,
    };
    let m = Measurement {
        quality,
        p99_overhead_ms: p99,
        false_positive_rate: error_rate,
        regression_count: 0,
        // Set truthfully below, only after a rollback is actually performed.
        rollback_verified: false,
        // Safety is the thermal invariant, and it is binary on purpose: the
        // AND-gate uses min-semantics, so a soft score here would let a hot box
        // buy its way through on quality.
        safety: if obs.thermally_stressed() { 0.0 } else { 1.0 },
        governance: 1.0,
    };
    (m, obs)
}

/// Run one full cycle.
pub fn run(samples: u32) -> anyhow::Result<serde_json::Value> {
    let mut b = backend();
    let applier = Applier::new(policy_path());
    let current = applier.load();
    // Resume the existing chain so this cycle's entries link to every prior
    // decision, rather than starting a disconnected log.
    let key = signing_key()?;
    let mut chain = match std::fs::read_to_string(chain_path()) {
        Ok(text) => Chain::resume(
            key,
            Chain::parse_jsonl(&text).map_err(|e| anyhow::anyhow!(e))?,
        ),
        Err(_) => Chain::new(key),
    };
    let already = chain.entries().len();
    let now = rultra_sense::now();

    // 1. Observe the parent.
    let (parent_m, obs) = observe(b.as_mut(), &current, samples);
    chain.append(
        now,
        Event::Observed {
            die_temp_c: obs.die_temp_c,
            read_error_rate: obs.read_error_rate,
            samples: obs.samples,
        },
        None,
        None,
    );

    let genome = genome_for(&current, !obs.thermally_stressed());

    // 2. Propose.
    let Some((mutation, candidate)) = propose(&genome, &current, &obs, now) else {
        persist(&chain, already)?;
        let out = json!({
            "decision": "no_change",
            "witness_verified": chain.verify(&chain.verifying_key()).is_ok(),
            "reason": "observation did not justify a change",
            "policy": current,
            "observation": {
                "die_temp_c": obs.die_temp_c,
                "read_error_rate": obs.read_error_rate,
                "samples": obs.samples,
            },
            "witness_entries": chain.entries().len(),
        });
        return Ok(out);
    };

    chain.append(
        now,
        Event::Proposed {
            mutation_id: mutation.id.clone(),
            parent_genome_hash: genome.hash.clone(),
            candidate_hash: candidate.content_hash(),
        },
        None,
        None,
    );

    // 3. Admission — autogenous's structural check, before anything is applied.
    if let Err(e) = mutation.admissible(&genome, now) {
        let reason = format!("inadmissible: {e:?}");
        chain.append(
            now,
            Event::Gated {
                mutation_id: mutation.id.clone(),
                passed: false,
                reason: reason.clone(),
            },
            None,
            None,
        );
        persist(&chain, already)?;
        return Ok(json!({ "decision": "refused", "reason": reason }));
    }

    // 4. Apply the candidate, then prove the rollback works BEFORE trusting it.
    //    autogenous requires rollback_verified, and that field means a rollback
    //    was executed and confirmed — not that one is theoretically available.
    let previous = applier.apply(&candidate)?;
    let rollback_verified = applier
        .rollback(&previous)
        .and_then(|_| applier.apply(&candidate).map(|_| ()))
        .is_ok();

    // 5. Observe the child.
    let (mut child_m, child_obs) = observe(b.as_mut(), &candidate, samples);
    child_m.rollback_verified = rollback_verified;
    chain.append(
        rultra_sense::now(),
        Event::Observed {
            die_temp_c: child_obs.die_temp_c,
            read_error_rate: child_obs.read_error_rate,
            samples: child_obs.samples,
        },
        None,
        None,
    );

    // 6. Score and gate.
    let verdict = score(&parent_m, &child_m, 0.05, now, &HardGates::default());
    let reason = format!(
        "delta_ci=[{:.3},{:.3}] beats_parent={} gates={} safety={:.2} rollback_verified={}",
        verdict.delta_lo,
        verdict.delta_hi,
        verdict.beats_parent,
        verdict.passes_gates,
        child_m.safety,
        rollback_verified
    );
    chain.append(
        rultra_sense::now(),
        Event::Gated {
            mutation_id: mutation.id.clone(),
            passed: verdict.promotable(),
            reason: reason.clone(),
        },
        None,
        None,
    );

    // 7. Promote or roll back.
    let decision = if verdict.promotable() {
        chain.append(
            rultra_sense::now(),
            Event::Promoted {
                mutation_id: mutation.id.clone(),
                genome_hash: candidate.content_hash(),
            },
            None,
            None,
        );
        "promoted"
    } else {
        applier.rollback(&previous)?;
        chain.append(
            rultra_sense::now(),
            Event::RolledBack {
                mutation_id: mutation.id.clone(),
                restored_hash: previous.content_hash(),
                verified: true,
            },
            None,
            None,
        );
        "rolled_back"
    };

    persist(&chain, already)?;
    let verified = chain.verify(&chain.verifying_key()).is_ok();

    Ok(json!({
        "decision": decision,
        "reason": reason,
        "mutation_id": mutation.id,
        "from": { "poll_interval_ms": current.poll_interval_ms },
        "to":   { "poll_interval_ms": candidate.poll_interval_ms },
        "active_policy": applier.load(),
        "observation": {
            "parent_die_temp_c": obs.die_temp_c,
            "child_die_temp_c": child_obs.die_temp_c,
        },
        "witness_entries": chain.entries().len(),
        "witness_verified": verified,
    }))
}

fn persist(chain: &Chain, already: usize) -> anyhow::Result<()> {
    let path = chain_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Append only what this run added; the file already holds the rest.
    let new: Vec<String> = chain
        .entries()
        .iter()
        .skip(already)
        .filter_map(|e| serde_json::to_string(e).ok())
        .collect();
    if new.is_empty() {
        return Ok(());
    }
    let mut body = new.join("\n");
    body.push('\n');
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(body.as_bytes())?;
    Ok(())
}
