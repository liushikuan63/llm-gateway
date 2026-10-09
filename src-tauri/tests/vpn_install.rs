//! Installation contracts use loopback HTTP and temporary files. Real ZIP acceptance is explicit.
use llm_gateway_lib::{
    vpn::{VpnManager, VpnProfileInput, VpnSettings},
    vpn_install::{self, InstalledKernel, KernelFallback, KernelInstallState, ENTRY_NAME},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("llmgw-vpn-install-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn settings(path: String) -> VpnSettings {
    let first = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let second = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    VpnSettings {
        kernel_path: path,
        mixed_port: first.local_addr().unwrap().port(),
        controller_port: second.local_addr().unwrap().port(),
    }
}

#[test]
fn pinned_release_and_redirect_policy_reject_untrusted_targets() {
    assert_eq!(vpn_install::VERSION, "v1.19.32");
    assert!(vpn_install::ASSET_URL.ends_with(vpn_install::ASSET_NAME));
    for url in [
        vpn_install::ASSET_URL,
        "https://release-assets.githubusercontent.com/fixture?signature=opaque",
        "https://objects.githubusercontent.com/fixture",
    ] {
        assert!(vpn_install::official_asset_url(
            &reqwest::Url::parse(url).unwrap()
        ));
    }
    for url in [
        "http://github.com/asset",
        "https://github.com.evil.invalid/asset",
        "https://evil.invalid/asset",
        "https://user:password@github.com/asset",
        "https://github.com:444/asset",
        "https://github.com/asset#fragment",
        "file:///fixture",
    ] {
        assert!(!vpn_install::official_asset_url(
            &reqwest::Url::parse(url).unwrap()
        ));
    }
    let directory = Directory::new();
    let escaped = InstalledKernel {
        directory: "../escaped".into(),
        version: vpn_install::VERSION.into(),
        sha256: vpn_install::EXE_SHA256.into(),
    };
    assert!(escaped.path(&directory.0).is_none());
    for name in ["", ".", "..", "C:\\escape", "dir/file", "dir\\file"] {
        assert!(InstalledKernel {
            directory: name.into(),
            ..escaped.clone()
        }
        .path(&directory.0)
        .is_none());
    }
}

#[test]
fn executable_digest_size_and_architecture_must_all_match() {
    let directory = Directory::new();
    let binary = directory.0.join("fixture.exe");
    let mut bytes = vec![0u8; 256];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&128u32.to_le_bytes());
    bytes[128..132].copy_from_slice(b"PE\0\0");
    bytes[132..134].copy_from_slice(&0x8664u16.to_le_bytes());
    bytes[152..154].copy_from_slice(&0x020bu16.to_le_bytes());
    fs::write(&binary, &bytes).unwrap();
    vpn_install::verify_hash(&binary, &digest(&bytes), 256).unwrap();
    vpn_install::verify_windows_amd64(&binary).unwrap();
    #[cfg(windows)]
    assert!(vpn_install::same_kernel_path(
        &binary,
        &PathBuf::from(binary.to_string_lossy().to_ascii_uppercase())
    ));
    assert!(vpn_install::verify_hash(&binary, &digest(&bytes), 255)
        .unwrap_err()
        .contains("大小上限"));
    assert!(vpn_install::verify_hash(&binary, &"0".repeat(64), 256)
        .unwrap_err()
        .contains("SHA256"));
    bytes[132..134].copy_from_slice(&0x014cu16.to_le_bytes());
    fs::write(&binary, &bytes).unwrap();
    assert!(vpn_install::verify_windows_amd64(&binary)
        .unwrap_err()
        .contains("amd64"));
    bytes[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&binary, bytes).unwrap();
    assert!(vpn_install::verify_windows_amd64(&binary).is_err());
}

fn loopback_fixture(url: &reqwest::Url) -> bool {
    url.scheme() == "http" && url.host_str() == Some("127.0.0.1")
}

#[tokio::test]
async fn transfer_is_bounded_rejects_redirects_and_cleans_interrupted_files() {
    use axum::{
        body::{Body, Bytes},
        http::{header, Response},
        routing::get,
        Router,
    };
    use futures_util::StreamExt;
    let router = Router::new()
        .route("/good", get(|| async { "fixed-download-fixture" }))
        .route(
            "/redirect",
            get(|| async {
                Response::builder()
                    .status(302)
                    .header(header::LOCATION, "https://untrusted.invalid/private-token")
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .route(
            "/large",
            get(|| async {
                Response::builder()
                    .header(
                        header::CONTENT_LENGTH,
                        (vpn_install::ZIP_LIMIT + 1).to_string(),
                    )
                    .body(Body::from_stream(futures_util::stream::iter([Ok::<
                        _,
                        std::io::Error,
                    >(
                        Bytes::from_static(b"x"),
                    )])))
                    .unwrap()
            }),
        )
        .route(
            "/cut",
            get(|| async {
                Response::builder()
                    .header(header::CONTENT_LENGTH, "100")
                    .body(Body::from_stream(futures_util::stream::iter([Ok::<
                        _,
                        std::io::Error,
                    >(
                        Bytes::from_static(b"partial"),
                    )])))
                    .unwrap()
            }),
        )
        .route(
            "/hold",
            get(|| async {
                Body::from_stream(
                    futures_util::stream::once(async {
                        Ok::<_, std::io::Error>(Bytes::from_static(b"initial"))
                    })
                    .chain(futures_util::stream::pending()),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let server = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
    let directory = Directory::new();
    let good = directory.0.join("good.zip");
    vpn_install::download_to_file(&format!("{base}/good"), &good, loopback_fixture)
        .await
        .unwrap();
    assert_eq!(fs::read(good).unwrap(), b"fixed-download-fixture");
    for endpoint in ["redirect", "large", "cut", "missing"] {
        let path = directory.0.join(format!("{endpoint}.zip"));
        let error =
            vpn_install::download_to_file(&format!("{base}/{endpoint}"), &path, loopback_fixture)
                .await
                .unwrap_err();
        assert!(!error.contains("private-token"));
        assert!(!path.exists(), "failed transfer left {endpoint} content");
    }
    let held = directory.0.join("held.zip");
    let task = {
        let held = held.clone();
        let url = format!("{base}/hold");
        tokio::spawn(
            async move { vpn_install::download_to_file(&url, &held, loopback_fixture).await },
        )
    };
    for _ in 0..500 {
        if held.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        held.exists(),
        "fixture must enter a real partial transfer before cancellation"
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!held.exists());
    server.abort();
    let unreachable = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = unreachable.local_addr().unwrap().port();
    drop(unreachable);
    let path = directory.0.join("offline.zip");
    assert!(vpn_install::download_to_file(
        &format!("http://127.0.0.1:{port}/asset"),
        &path,
        loopback_fixture
    )
    .await
    .is_err());
    assert!(!path.exists());
}

fn fixture_zip(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    fn word(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    fn long(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, bytes, attributes) in entries {
        let offset = out.len() as u32;
        let mut crc = !0u32;
        for byte in *bytes {
            crc ^= *byte as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        crc = !crc;
        long(&mut out, 0x04034b50);
        for value in [20, 0, 0, 0, 0] {
            word(&mut out, value);
        }
        for value in [crc, bytes.len() as u32, bytes.len() as u32] {
            long(&mut out, value);
        }
        word(&mut out, name.len() as u16);
        word(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(bytes);
        long(&mut central, 0x02014b50);
        for value in [20, 20, 0, 0, 0, 0] {
            word(&mut central, value);
        }
        for value in [crc, bytes.len() as u32, bytes.len() as u32] {
            long(&mut central, value);
        }
        for value in [name.len() as u16, 0, 0, 0, 0] {
            word(&mut central, value);
        }
        long(&mut central, *attributes);
        long(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let offset = out.len() as u32;
    out.extend_from_slice(&central);
    long(&mut out, 0x06054b50);
    for value in [0, 0, entries.len() as u16, entries.len() as u16] {
        word(&mut out, value);
    }
    long(&mut out, central.len() as u32);
    long(&mut out, offset);
    word(&mut out, 0);
    out
}

#[cfg(windows)]
#[tokio::test]
async fn zip_extraction_only_accepts_one_exact_regular_entry() {
    let directory = Directory::new();
    let cases = [
        vec![(ENTRY_NAME, b"regular".as_slice(), 0)],
        vec![("../escape.exe", b"unsafe".as_slice(), 0)],
        vec![("C:\\escape.exe", b"unsafe".as_slice(), 0)],
        vec![("/escape.exe", b"unsafe".as_slice(), 0)],
        vec![(ENTRY_NAME, b"link".as_slice(), 0xa0000000)],
        vec![
            (ENTRY_NAME, b"one".as_slice(), 0),
            (ENTRY_NAME, b"two".as_slice(), 0),
        ],
    ];
    for (index, entries) in cases.iter().enumerate() {
        let archive = directory.0.join(format!("fixture-{index}.zip"));
        let binary = directory.0.join(format!("result-{index}.exe"));
        fs::write(&archive, fixture_zip(entries)).unwrap();
        let result = vpn_install::extract_verified_zip(
            &archive,
            &binary,
            &directory.0.join(format!("extract-{index}.ps1")),
            &AtomicBool::new(false),
            Arc::new(parking_lot::Mutex::new(None)),
        )
        .await;
        if index == 0 {
            result.unwrap();
            assert_eq!(fs::read(binary).unwrap(), b"regular");
        } else {
            assert!(result.is_err());
            assert!(!binary.exists());
        }
    }
    assert!(!directory.0.join("escape.exe").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn corrupt_bundled_archive_is_rejected_without_touching_selected_user_file() {
    let directory = Directory::new();
    let user = directory.0.join("user-selected.exe");
    fs::write(&user, b"keep-user-file").unwrap();
    let archive = directory.0.join("corrupt.zip");
    fs::write(&archive, fixture_zip(&[(ENTRY_NAME, b"fixture", 0)])).unwrap();
    let root = directory.0.join("owned");
    let manager = VpnManager::with_bundled_archive(root.clone(), archive);
    let original = settings(user.to_string_lossy().into());
    manager.save_settings(original.clone()).await.unwrap();
    let error = manager.install_kernel().await.unwrap_err();
    assert!(error.contains("SHA256"));
    assert_eq!(manager.status().await.unwrap().settings, original);
    assert_eq!(fs::read(user).unwrap(), b"keep-user-file");
    assert_eq!(fs::read_dir(root.join("kernels")).unwrap().count(), 0);
    assert!(!root.join("runtime/config.json").exists());
    assert!(!manager.kernel_info().await.unwrap().installed);
}

fn recorded_install(
    directory: &Directory,
    pending: bool,
) -> (PathBuf, PathBuf, KernelInstallState, VpnSettings) {
    let root = directory.0.join("owned");
    let previous = directory.0.join("user-previous.exe");
    fs::write(&previous, b"do-not-overwrite-user-binary").unwrap();
    let kernel = InstalledKernel {
        directory: "v1.19.32-fixture".into(),
        version: vpn_install::VERSION.into(),
        sha256: digest(b"invalid-new-core"),
    };
    let current = kernel.path(&root).unwrap();
    fs::create_dir_all(current.parent().unwrap()).unwrap();
    fs::write(&current, b"invalid-new-core").unwrap();
    let installs = KernelInstallState {
        current: Some(kernel),
        previous: Some(KernelFallback {
            path: previous.to_string_lossy().into(),
            managed: None,
        }),
        pending,
    };
    let configured = settings(current.to_string_lossy().into());
    let mut value = serde_json::to_value(&configured).unwrap();
    value["kernel_install"] = serde_json::to_value(&installs).unwrap();
    vpn_install::atomic_write(
        &root.join("settings.json"),
        &serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    (root, previous, installs, configured)
}

#[tokio::test]
async fn explicit_rollback_and_manual_selection_keep_files_and_persist_one_journal() {
    let directory = Directory::new();
    let (root, previous, installs, configured) = recorded_install(&directory, true);
    let manager = VpnManager::new(root.clone());
    let info = manager.kernel_info().await.unwrap();
    assert!(info.managed && info.can_rollback);
    assert!(
        !info.installed,
        "fixture digest cannot count as pinned official installation"
    );
    let rolled_back = manager.rollback_kernel().await.unwrap();
    assert_eq!(rolled_back.settings.kernel_path, previous.to_string_lossy());
    assert!(!rolled_back.running);
    assert_eq!(
        fs::read(&previous).unwrap(),
        b"do-not-overwrite-user-binary"
    );
    assert!(installs.current.unwrap().path(&root).unwrap().exists());
    let info = manager.kernel_info().await.unwrap();
    assert!(!info.managed && !info.can_rollback);
    assert!(manager.rollback_kernel().await.is_err());
    let reloaded = VpnManager::new(root.clone());
    assert_eq!(
        reloaded.status().await.unwrap().settings,
        rolled_back.settings
    );
    reloaded.save_settings(configured).await.unwrap();
    let stored: Value =
        serde_json::from_slice(&fs::read(root.join("settings.json")).unwrap()).unwrap();
    assert_eq!(stored["kernel_install"]["pending"], false);
    assert_eq!(
        fs::read_dir(&root)
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
            .count(),
        0
    );
}

#[tokio::test]
async fn first_new_core_start_failure_restores_previous_selection_without_starting_it() {
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
    );
    let directory = Directory::new();
    let (root, previous, _, _) = recorded_install(&directory, true);
    let profile = directory.0.join("fixture.yaml");
    fs::write(&profile, "proxies: []\n").unwrap();
    let manager = VpnManager::new(root.clone());
    manager
        .import_profile(VpnProfileInput {
            path: Some(profile.to_string_lossy().into()),
            url: None,
        })
        .await
        .unwrap();
    assert!(manager
        .start()
        .await
        .unwrap_err()
        .contains("已恢复前版内核选择"));
    let status = manager.status().await.unwrap();
    assert!(!status.running);
    assert_eq!(status.settings.kernel_path, previous.to_string_lossy());
    assert!(status.error.unwrap().contains("VPN 保持停止"));
    assert!(!root.join("runtime/nodes.yaml").exists());
    assert!(!root.join("runtime/config.json").exists());
    assert_eq!(fs::read(previous).unwrap(), b"do-not-overwrite-user-binary");
}

#[tokio::test]
async fn readonly_snapshot_does_not_delete_files_or_modify_saved_settings() {
    let directory = Directory::new();
    let root = directory.0.join("owned");
    let manager = VpnManager::new(root.clone());
    manager.save_settings(VpnSettings::default()).await.unwrap();
    fs::create_dir(root.join("runtime")).unwrap();
    fs::write(root.join("runtime/config.json"), b"fixture-controller").unwrap();
    fs::write(root.join("runtime/nodes.yaml"), b"fixture-profile").unwrap();
    let saved = fs::read(root.join("settings.json")).unwrap();
    let snapshot = manager.diagnostics_snapshot().unwrap();
    assert!(!snapshot.running && !snapshot.has_error);
    assert_eq!(snapshot.mixed_port, 17890);
    assert_eq!(fs::read(root.join("settings.json")).unwrap(), saved);
    assert_eq!(
        fs::read(root.join("runtime/config.json")).unwrap(),
        b"fixture-controller"
    );
    assert_eq!(
        fs::read(root.join("runtime/nodes.yaml")).unwrap(),
        b"fixture-profile"
    );
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "requires LLMGW_TEST_MIHOMO_ZIP; verified official archive only, no download or external nodes"]
async fn real_bundled_archive_install_cancel_rollback_and_first_start_recovery() {
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
    );
    let archive = PathBuf::from(
        std::env::var("LLMGW_TEST_MIHOMO_ZIP").expect("set fixed verified official ZIP"),
    );
    vpn_install::verify_hash(&archive, vpn_install::ZIP_SHA256, vpn_install::ZIP_LIMIT).unwrap();
    let directory = Directory::new();
    let root = directory.0.join("owned");
    let previous = directory.0.join("user-previous.exe");
    fs::write(&previous, b"user-file-preserved").unwrap();
    let manager = Arc::new(VpnManager::with_bundled_archive(
        root.clone(),
        archive.clone(),
    ));
    let original = settings(previous.to_string_lossy().into());
    let tampered = directory.0.join("tampered-official.zip");
    let mut tampered_bytes = fs::read(&archive).unwrap();
    tampered_bytes.extend_from_slice(b"changed-archive");
    fs::write(&tampered, tampered_bytes).unwrap();
    let corrupt_root = directory.0.join("corrupt-official");
    let corrupt_manager = VpnManager::with_bundled_archive(corrupt_root.clone(), tampered);
    corrupt_manager
        .save_settings(original.clone())
        .await
        .unwrap();
    assert!(corrupt_manager
        .install_kernel()
        .await
        .unwrap_err()
        .contains("SHA256"));
    assert_eq!(corrupt_manager.status().await.unwrap().settings, original);
    assert_eq!(
        fs::read_dir(corrupt_root.join("kernels")).unwrap().count(),
        0
    );
    manager.save_settings(original.clone()).await.unwrap();
    let installing = {
        let manager = manager.clone();
        tokio::spawn(async move { manager.install_kernel().await })
    };
    for _ in 0..100 {
        if root.join("kernels").is_dir() && fs::read_dir(root.join("kernels")).unwrap().count() > 0
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        fs::read_dir(root.join("kernels")).unwrap().count() > 0,
        "cancellation must enter the installation stage"
    );
    manager.shutdown_blocking();
    assert!(installing.await.unwrap().is_err());
    assert_eq!(fs::read_dir(root.join("kernels")).unwrap().count(), 0);
    drop(manager);
    let manager = VpnManager::with_bundled_archive(root.clone(), archive);
    let installed = manager.install_kernel().await.unwrap();
    assert!(!installed.running);
    assert_ne!(installed.settings.kernel_path, original.kernel_path);
    let info = manager.kernel_info().await.unwrap();
    assert!(info.installed && info.managed && info.can_rollback);
    assert_eq!(info.version, "v1.19.32");
    let same = manager
        .install_kernel_from_verified_archive(&directory.0.join("not-used.zip"))
        .await
        .unwrap();
    assert_eq!(same.settings.kernel_path, installed.settings.kernel_path);
    assert_eq!(fs::read_dir(root.join("kernels")).unwrap().count(), 1);
    manager.rollback_kernel().await.unwrap();
    assert_eq!(manager.status().await.unwrap().settings, original);
    manager.install_kernel().await.unwrap();
    let profile = directory.0.join("invalid.yaml");
    fs::write(&profile, "proxies: []\n").unwrap();
    manager
        .import_profile(VpnProfileInput {
            path: Some(profile.to_string_lossy().into()),
            url: None,
        })
        .await
        .unwrap();
    assert!(manager
        .start()
        .await
        .unwrap_err()
        .contains("已恢复前版内核选择"));
    let status = manager.status().await.unwrap();
    assert!(!status.running);
    assert_eq!(status.settings.kernel_path, original.kernel_path);
    assert_eq!(fs::read(previous).unwrap(), b"user-file-preserved");
    assert!(!root.join("runtime/nodes.yaml").exists());
    assert!(!root.join("runtime/config.json").exists());
    println!("official bundled v1.19.32 ZIP/exe/PE/version installation, cancellation and rollback verified");
}
