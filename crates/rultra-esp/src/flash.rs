//! Flash operations that cannot run without a backup, and image checks that
//! catch the mistake that actually happened.
//!
//! Two safeguards are encoded here because both were learned the expensive
//! way during the first flash of a board on this box:
//!
//! 1. **A flash cannot be planned without a backup.** [`FlashPlan`] has no
//!    constructor that does not take a [`Backup`], so "I'll take the backup
//!    afterwards" is not expressible. Erasing a board is one-way; the vendor
//!    firmware that was on it may not be downloadable anywhere.
//!
//! 2. **An app-only image is not a factory image.** The Tasmota webcam
//!    download is 1,376,656 bytes of *application*, meant for an existing
//!    partition layout. The board's own layout gave `app0` only 0x140000
//!    (1,310,720) bytes — 66 KB too small. Writing it at 0x10000 would have
//!    overrun into `app1` and produced a board that neither booted nor
//!    explained why. [`ImageKind::detect`] tells the two apart from the bytes,
//!    and [`FlashPlan::app`] refuses a write that does not fit.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// ESP image magic: every image segment header starts with this byte.
const ESP_MAGIC: u8 = 0xE9;
/// Offset of the bootloader in a flashed ESP32 image.
const BOOTLOADER_OFFSET: usize = 0x1000;
/// Offset of the partition table.
const PARTITION_TABLE_OFFSET: usize = 0x8000;
/// Partition entry magic, little-endian 0x50AA.
const PARTITION_MAGIC: [u8; 2] = [0xAA, 0x50];
const PARTITION_ENTRY_LEN: usize = 32;

/// What a `.bin` file actually is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageKind {
    /// Bootloader + partition table + app, written at offset 0. Self-contained:
    /// it brings its own partition layout, so it does not care what was there.
    Factory,
    /// Application only. Must be written into an existing app partition, and
    /// only fits if that partition is large enough.
    AppOnly,
    NotAnEspImage,
}

impl ImageKind {
    pub fn detect(bytes: &[u8]) -> Self {
        let has_partition_table = bytes.len() > PARTITION_TABLE_OFFSET + 2
            && bytes[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 2] == PARTITION_MAGIC;
        let has_bootloader =
            bytes.len() > BOOTLOADER_OFFSET && bytes[BOOTLOADER_OFFSET] == ESP_MAGIC;
        if has_partition_table && has_bootloader {
            return ImageKind::Factory;
        }
        if bytes.first() == Some(&ESP_MAGIC) {
            return ImageKind::AppOnly;
        }
        ImageKind::NotAnEspImage
    }

    /// The offset this image must be written at, if it can be written at all.
    pub fn required_offset(self) -> Option<u32> {
        match self {
            ImageKind::Factory => Some(0),
            // An app image's offset comes from the partition table, not the file.
            ImageKind::AppOnly | ImageKind::NotAnEspImage => None,
        }
    }
}

/// One entry from the on-device partition table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    pub label: String,
    pub offset: u32,
    pub size: u32,
}

/// Parse the partition table out of a full flash image (such as a backup).
pub fn parse_partition_table(flash: &[u8]) -> Vec<Partition> {
    let mut out = Vec::new();
    let mut at = PARTITION_TABLE_OFFSET;
    while at + PARTITION_ENTRY_LEN <= flash.len() {
        let e = &flash[at..at + PARTITION_ENTRY_LEN];
        if e[0..2] != PARTITION_MAGIC {
            break;
        }
        let offset = u32::from_le_bytes([e[4], e[5], e[6], e[7]]);
        let size = u32::from_le_bytes([e[8], e[9], e[10], e[11]]);
        let label = String::from_utf8_lossy(&e[12..28])
            .trim_end_matches('\0')
            .trim()
            .to_string();
        out.push(Partition {
            label,
            offset,
            size,
        });
        at += PARTITION_ENTRY_LEN;
    }
    out
}

/// Proof that a backup was taken. Only obtainable by actually producing one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    pub path: PathBuf,
    pub bytes: u64,
    /// Recorded when the caller computed one. Absent means "not verified",
    /// never "verified empty".
    pub sha256: Option<String>,
}

impl Backup {
    /// Record a backup, verifying the file exists and covers the whole chip.
    ///
    /// A short read is rejected: a truncated backup is worse than none,
    /// because it looks like a safety net and is not one.
    pub fn record(path: &Path, expected_bytes: u64, sha256: Option<String>) -> Result<Backup> {
        let meta = std::fs::metadata(path)
            .with_context(|| format!("backup file missing: {}", path.display()))?;
        if meta.len() == 0 {
            bail!("backup {} is empty", path.display());
        }
        if meta.len() < expected_bytes {
            bail!(
                "backup {} is {} bytes but the chip holds {} — a partial backup is not a backup",
                path.display(),
                meta.len(),
                expected_bytes
            );
        }
        Ok(Backup {
            path: path.to_path_buf(),
            bytes: meta.len(),
            sha256,
        })
    }
}

/// A checked, ready-to-execute flash operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashPlan {
    pub image: PathBuf,
    pub offset: u32,
    pub kind: ImageKind,
    /// Non-optional on purpose. See the module docs.
    pub backup: Backup,
}

impl FlashPlan {
    /// Plan a factory write at offset 0.
    pub fn factory(image: &Path, bytes: &[u8], backup: Backup) -> Result<FlashPlan> {
        match ImageKind::detect(bytes) {
            ImageKind::Factory => Ok(FlashPlan {
                image: image.to_path_buf(),
                offset: 0,
                kind: ImageKind::Factory,
                backup,
            }),
            other => bail!(
                "{} is {:?}, not a factory image — writing it at 0x0 would leave the \
                 board with no bootloader",
                image.display(),
                other
            ),
        }
    }

    /// Plan an application write into an existing partition, checking that it
    /// fits before anything is erased.
    pub fn app(
        image: &Path,
        bytes: &[u8],
        backup: Backup,
        target: &Partition,
    ) -> Result<FlashPlan> {
        let kind = ImageKind::detect(bytes);
        if kind != ImageKind::AppOnly {
            bail!(
                "{} is {:?}, not an application image",
                image.display(),
                kind
            );
        }
        if bytes.len() as u64 > target.size as u64 {
            bail!(
                "image is {} bytes but partition '{}' holds only {} — short by {} bytes",
                bytes.len(),
                target.label,
                target.size,
                bytes.len() as u64 - target.size as u64
            );
        }
        Ok(FlashPlan {
            image: image.to_path_buf(),
            offset: target.offset,
            kind,
            backup,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real Freenove partition table, as read back from the board.
    fn freenove_flash() -> Vec<u8> {
        let mut f = vec![0u8; PARTITION_TABLE_OFFSET + 5 * PARTITION_ENTRY_LEN];
        let mut put = |i: usize, ty: u8, sub: u8, off: u32, size: u32, label: &str| {
            let at = PARTITION_TABLE_OFFSET + i * PARTITION_ENTRY_LEN;
            f[at..at + 2].copy_from_slice(&PARTITION_MAGIC);
            f[at + 2] = ty;
            f[at + 3] = sub;
            f[at + 4..at + 8].copy_from_slice(&off.to_le_bytes());
            f[at + 8..at + 12].copy_from_slice(&size.to_le_bytes());
            f[at + 12..at + 12 + label.len()].copy_from_slice(label.as_bytes());
        };
        put(0, 1, 2, 0x9000, 0x5000, "nvs");
        put(1, 1, 0, 0xe000, 0x2000, "otadata");
        put(2, 0, 0x10, 0x10000, 0x140000, "app0");
        put(3, 0, 0x11, 0x150000, 0x140000, "app1");
        put(4, 1, 0x82, 0x290000, 0x160000, "spiffs");
        f
    }

    fn a_backup() -> Backup {
        Backup {
            path: "/tmp/b.bin".into(),
            bytes: 4 * 1024 * 1024,
            sha256: None,
        }
    }

    #[test]
    fn the_real_partition_table_parses() {
        let p = parse_partition_table(&freenove_flash());
        assert_eq!(p.len(), 5);
        assert_eq!(
            p[2],
            Partition {
                label: "app0".into(),
                offset: 0x10000,
                size: 0x140000
            }
        );
        assert_eq!(p[4].label, "spiffs");
    }

    #[test]
    fn a_factory_image_is_told_apart_from_an_app_image() {
        let mut factory = vec![0xFFu8; 0x9000];
        factory[BOOTLOADER_OFFSET] = ESP_MAGIC;
        factory[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 2]
            .copy_from_slice(&PARTITION_MAGIC);
        assert_eq!(ImageKind::detect(&factory), ImageKind::Factory);

        let mut app = vec![0u8; 0x9000];
        app[0] = ESP_MAGIC;
        assert_eq!(ImageKind::detect(&app), ImageKind::AppOnly);

        assert_eq!(ImageKind::detect(&[0u8; 64]), ImageKind::NotAnEspImage);
    }

    #[test]
    fn the_tasmota_app_is_refused_because_it_does_not_fit_app0() {
        // The actual numbers: 1,376,656 bytes into a 1,310,720-byte partition.
        let mut app = vec![0u8; 1_376_656];
        app[0] = ESP_MAGIC;
        let parts = parse_partition_table(&freenove_flash());
        let app0 = parts.iter().find(|p| p.label == "app0").unwrap();
        let err = FlashPlan::app(Path::new("t.bin"), &app, a_backup(), app0).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("short by 65936"), "unhelpful error: {msg}");
    }

    #[test]
    fn a_factory_image_goes_to_offset_zero() {
        let mut factory = vec![0xFFu8; 0x9000];
        factory[BOOTLOADER_OFFSET] = ESP_MAGIC;
        factory[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 2]
            .copy_from_slice(&PARTITION_MAGIC);
        let plan = FlashPlan::factory(Path::new("f.bin"), &factory, a_backup()).unwrap();
        assert_eq!(plan.offset, 0);
    }

    #[test]
    fn an_app_image_is_refused_at_offset_zero() {
        let mut app = vec![0u8; 0x9000];
        app[0] = ESP_MAGIC;
        assert!(FlashPlan::factory(Path::new("a.bin"), &app, a_backup()).is_err());
    }

    #[test]
    fn a_truncated_backup_is_rejected() {
        let dir = std::env::temp_dir().join("rultra-esp-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("short.bin");
        std::fs::write(&p, vec![0u8; 1024]).unwrap();
        let err = Backup::record(&p, 4 * 1024 * 1024, None).unwrap_err();
        assert!(err.to_string().contains("not a backup"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_missing_backup_cannot_be_recorded() {
        assert!(Backup::record(Path::new("/nonexistent/x.bin"), 1, None).is_err());
    }
}
