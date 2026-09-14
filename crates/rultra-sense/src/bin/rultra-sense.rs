//! `rultra-sense` — one unified sensing surface, as a command.
//!
//! ```text
//! rultra-sense inventory     what the board carries, and how well it is known
//! rultra-sense probe         which devices answer right now
//! rultra-sense stream [ms]   JSON-lines telemetry from every responding device
//! rultra-sense matrix <what>  drive the 8x8 LED matrix:
//!                             heart | clear | test | scroll <text> [frame_ms]
//! rultra-sense lcd <what>     drive the 16x2 LCD: write <l1> [l2] | backlight on|off
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
                    Verification::Unvalidated => "uncal",
                    Verification::AcksButSilent => "acks",
                    Verification::Untested => "??  ",
                    Verification::Faulty => "BAD ",
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
        #[cfg(all(target_os = "linux", feature = "hardware"))]
        "matrix" => {
            use rultra_sense::backend::linux::LinuxBackend;
            const HEART: [u8; 8] = [0x00, 0x66, 0xff, 0xff, 0xff, 0x7e, 0x3c, 0x18];
            match args.get(2).map(String::as_str).unwrap_or("heart") {
                "heart" => LinuxBackend::matrix_draw(&HEART)?,
                "clear" => LinuxBackend::matrix_draw(&[0; 8])?,
                "test" => {
                    for _ in 0..5 {
                        LinuxBackend::matrix_display_test(true)?;
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        LinuxBackend::matrix_display_test(false)?;
                        std::thread::sleep(std::time::Duration::from_millis(400));
                    }
                }
                "scroll" => {
                    let text = args.get(3).cloned().unwrap_or_else(|| "RULTRA".to_string());
                    let ms = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(60);
                    LinuxBackend::matrix_scroll(&text, ms)?;
                }
                w => anyhow::bail!("unknown matrix pattern: {w}"),
            }
        }
        #[cfg(all(target_os = "linux", feature = "hardware"))]
        "lcd" => {
            use rultra_sense::backend::linux::LinuxBackend;
            let b = LinuxBackend::open()?;
            match args.get(2).map(String::as_str).unwrap_or("write") {
                "backlight" => {
                    let on = args.get(3).map(String::as_str) != Some("off");
                    b.lcd_backlight(on)?;
                }
                "write" => b.lcd_write(
                    args.get(3).map(String::as_str).unwrap_or("rultra"),
                    args.get(4).map(String::as_str).unwrap_or(""),
                )?,
                w => anyhow::bail!("unknown lcd command: {w}"),
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
