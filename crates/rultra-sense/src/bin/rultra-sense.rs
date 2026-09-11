//! `rultra-sense` — one unified sensing surface, as a command.
//!
//! ```text
//! rultra-sense inventory     what the board carries, and how well it is known
//! rultra-sense probe         which devices answer right now
//! rultra-sense stream [ms]   JSON-lines telemetry from every responding device
//! ```
//!
//! Output is JSON Lines on stdout so it pipes into anything. Diagnostics go to
//! stderr, keeping stdout a clean data stream.

use rultra_sense::{backend::mock::MockBackend, device, Backend, DeviceKind, Verification};

fn backend() -> Box<dyn Backend> {
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    {
        match rultra_sense::backend::linux::LinuxBackend::open() {
            Ok(b) => return Box::new(b),
            Err(e) => eprintln!("hardware backend unavailable ({e}); using mock"),
        }
    }
    Box::new(MockBackend::crowpi())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("inventory") {
        "inventory" => {
            for d in device::CATALOG {
                let mark = match d.verification {
                    Verification::Working => "ok  ",
                    Verification::AcksButSilent => "acks",
                    Verification::Untested => "??  ",
                };
                println!(
                    "{mark} {:<16} {:<22} {}",
                    format!("{:?}", d.id),
                    d.part,
                    d.evidence
                );
            }
        }
        "probe" => {
            for p in backend().probe()? {
                println!("{}", serde_json::to_string(&p)?);
            }
        }
        "stream" => {
            let period = args
                .get(2)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(1000);
            let mut b = backend();
            let live: Vec<_> = b
                .probe()?
                .into_iter()
                .filter(|p| p.responding)
                .map(|p| p.device)
                // Only sensors can be streamed; driving an output is a
                // different verb entirely.
                .filter(|id| device::lookup(*id).map(|d| d.kind) == Some(DeviceKind::Sensor))
                .collect();
            if live.is_empty() {
                eprintln!("no devices responding; nothing to stream");
                return Ok(());
            }
            eprintln!("streaming {} device(s) every {period}ms", live.len());
            loop {
                for id in &live {
                    match b.read(*id) {
                        Ok(r) => println!("{}", serde_json::to_string(&r)?),
                        Err(e) => eprintln!("{id:?}: {e}"),
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(period));
            }
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: rultra-sense inventory|probe|stream [period_ms]");
            std::process::exit(2);
        }
    }
    Ok(())
}
