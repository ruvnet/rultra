//! `rultra-spatial` — render the room on the matrix, live.
//!
//! ```text
//! rultra-spatial watch [seconds]   drive the matrix from the sensors
//! rultra-spatial once              print one fused state and its steering
//! ```
use rultra_sense::{Backend, DeviceId, Value, Verification};
use rultra_spatial::{RoomState, Steering};

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

fn scalar(b: &mut dyn Backend, id: DeviceId) -> Option<f64> {
    match b.read(id).ok()?.value {
        Value::Scalar { n, .. } => Some(n),
        _ => None,
    }
}

fn verification(id: DeviceId) -> Verification {
    rultra_sense::device::lookup(id)
        .map(|d| d.verification)
        .unwrap_or(Verification::Untested)
}

fn observe(b: &mut dyn Backend, prev: Option<&RoomState>) -> RoomState {
    let range_m = scalar(b, DeviceId::Range).map(|cm| cm / 100.0);
    let lux = scalar(b, DeviceId::Light);
    RoomState::fuse(
        range_m,
        lux,
        verification(DeviceId::Range),
        verification(DeviceId::Light),
        prev,
    )
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut b = backend();

    match args.get(1).map(String::as_str).unwrap_or("once") {
        "once" => {
            let room = observe(b.as_mut(), None);
            println!("{}", serde_json::to_string_pretty(&room)?);
            println!("{}", serde_json::to_string_pretty(&Steering::from(&room))?);
        }
        "watch" => {
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            let mut prev: Option<RoomState> = None;
            while std::time::Instant::now() < deadline {
                let room = observe(b.as_mut(), prev.as_ref());
                #[cfg(all(target_os = "linux", feature = "hardware"))]
                {
                    use rultra_sense::backend::linux::LinuxBackend;
                    use rultra_spatial::visual;
                    let _ = LinuxBackend::matrix_draw_with_intensity(
                        &visual::render(&room),
                        visual::intensity(&room),
                    );
                }
                let s = Steering::from(&room);
                println!(
                    "{:?}/{:?}  range={:>6}  lux={:>6}  intensity={:.2} calm={:.2} billable={}",
                    room.proximity,
                    room.light,
                    room.range_m
                        .map(|m| format!("{:.2}m", m))
                        .unwrap_or("-".into()),
                    room.lux.map(|l| format!("{:.0}", l)).unwrap_or("-".into()),
                    s.intensity,
                    s.calm,
                    s.billable
                );
                prev = Some(room);
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
            // Leave the panel dark rather than frozen on the last frame: a
            // static figure reads as a live reading that stopped updating.
            #[cfg(all(target_os = "linux", feature = "hardware"))]
            {
                let _ = rultra_sense::backend::linux::LinuxBackend::matrix_draw(&[0u8; 8]);
            }
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: rultra-spatial once|watch [seconds]");
            std::process::exit(2);
        }
    }
    Ok(())
}
