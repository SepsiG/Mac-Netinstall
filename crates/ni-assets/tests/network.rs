//! Opt-in checks against the real MikroTik servers (downloads ~55 MB).
//!
//! ```text
//! cargo test -p ni-assets --test network -- --ignored --test-threads=1
//! ```
//!
//! `local_netinstall_cli_carves_the_known_kernels` reads a local copy of
//! `netinstall-cli` instead; set `NI_ASSETS_NETINSTALL_CLI=/path/to/netinstall-cli`.

use ni_assets::npk::sha256_hex;
use ni_assets::{extract_kernels, Assets, Channel, HttpConfig, KernelSet, PkgArch, Version};
use ni_bootp::BootArch;
use std::path::{Path, PathBuf};

/// Kernels carved from `netinstall-cli` 7.25.1: (arch, size, SHA-256),
/// computed independently with a Python ELF-header script.
const KERNELS_7_25_1: [(BootArch, usize, &str); 8] = [
    (
        BootArch::Mips,
        5_256_784,
        "f21f554ae753bcb5ce34ed701d258152d07ddb5d49c2055e0a4ba6ff46c422c3",
    ),
    (
        BootArch::MMips,
        5_358_860,
        "c3dacc3cb8713f512ee5ce9a073f0f83912f5ad735755f66252604f651d9b0d6",
    ),
    (
        BootArch::PowerPc,
        5_486_936,
        "76cad794e2f97bb3ffe67fe5e9df362314e9785cb54628a543913f257f68e3cd",
    ),
    (
        BootArch::E500,
        6_119_156,
        "860de221e5810c4d19fd387fc7866b2e62482506e70cf7764164f48b9ec3c4f9",
    ),
    (
        BootArch::Ppc440,
        5_709_072,
        "192eb874da5f568f8a587be87d162099f41e86567a0e917737585a3b6fa59c0a",
    ),
    (
        BootArch::Arm,
        3_501_676,
        "8e86029a0aeffc02f81996e1baece158941b5dd4a4a7811a03c2f009b8b2464f",
    ),
    (
        BootArch::Arm64,
        7_838_328,
        "f6fe92aec57fd42e2806580195bb592aa63efc4d3dde2be8814876aa43629568",
    ),
    (
        BootArch::Tile,
        4_680_160,
        "1729c62a2bc6cc931f9ddb58ed59eeabb8f192ef4ea41c38e47473a98c88eb35",
    ),
];

fn check_kernels(set: &KernelSet) {
    assert_eq!(set.len(), 8);
    for (arch, size, sha) in KERNELS_7_25_1 {
        let image = &set[&arch];
        assert_eq!(
            (image.len(), sha256_hex(image).as_str()),
            (size, sha),
            "{arch}"
        );
    }
}

/// A cache root under the system temp dir, removed on drop (kept only if
/// the test panics and `NI_ASSETS_KEEP_TEST_DIRS=1`).
struct TempRoot(PathBuf);

impl std::ops::Deref for TempRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let keep = std::thread::panicking()
            && std::env::var("NI_ASSETS_KEEP_TEST_DIRS").is_ok_and(|v| v == "1");
        if !keep {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn temp_root(tag: &str) -> TempRoot {
    let dir = std::env::temp_dir().join(format!("ni-assets-net-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    TempRoot(dir)
}

fn v7251() -> Version {
    Version::parse("7.25.1").unwrap()
}

#[tokio::test]
#[ignore = "network"]
async fn real_channels_resolve() {
    let root = temp_root("channels");
    let assets = Assets::new(&*root, HttpConfig::default()).unwrap();
    for channel in Channel::ALL {
        let release = assets.latest(channel).await.unwrap();
        assert!(
            release.released_unix > 1_600_000_000,
            "{channel}: {release:?}"
        );
        println!("{channel}: {} ({})", release.version, release.released_unix);
    }
}

#[tokio::test]
#[ignore = "network"]
async fn real_7_25_1_kernels_match_the_reference() {
    let root = temp_root("kernels");
    let assets = Assets::new(&*root, HttpConfig::default()).unwrap();
    let set = assets.ensure_kernels(&v7251()).await.unwrap();
    check_kernels(&set);
    let index = ni_assets::cache::CacheDir::new(root.join("7.25.1")).load();
    assert!(index.files["kernel-arm64.elf"].published_checksum);
    // From the cache this time.
    check_kernels(&assets.ensure_kernels(&v7251()).await.unwrap());
}

#[tokio::test]
#[ignore = "network"]
async fn real_7_25_1_smips_packages() {
    let root = temp_root("smips");
    let assets = Assets::new(&*root, HttpConfig::default()).unwrap();
    let names = assets
        .available_packages(&v7251(), PkgArch::Smips)
        .await
        .unwrap();
    assert_eq!(names, ["hotspot", "tr069-client", "wireless"]);
    let files = assets
        .ensure_packages(&v7251(), PkgArch::Smips, &["wireless".into()])
        .await
        .unwrap();
    let got: Vec<(&str, usize)> = files
        .iter()
        .map(|f| (f.name.as_str(), f.data.len()))
        .collect();
    assert_eq!(
        got,
        [
            ("routeros-7.25.1-smips.npk", 7_092_728),
            ("wireless-7.25.1-smips.npk", 671_889)
        ]
    );
    assert_eq!(
        sha256_hex(&files[0].data),
        "3150ede39c629a0e4d1b0eeb72e1aad34fbe1756b4b9a38af32b6a09d792988e"
    );
    let plan = ni_proto::InstallPlan {
        packages: files,
        config_script: None,
    };
    assert_eq!(plan.validate(), Ok(()));
}

#[test]
#[ignore = "needs NI_ASSETS_NETINSTALL_CLI"]
fn local_netinstall_cli_carves_the_known_kernels() {
    let path = std::env::var_os("NI_ASSETS_NETINSTALL_CLI")
        .expect("set NI_ASSETS_NETINSTALL_CLI to a 7.25.1 netinstall-cli");
    let data = std::fs::read(path).unwrap();
    check_kernels(&extract_kernels(&data).unwrap());
}
