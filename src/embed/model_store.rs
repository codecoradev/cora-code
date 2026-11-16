//! The code embedding model on disk: where it lives, how it is fetched once, and
//! how it is verified.
//!
//! cora's default embedding is `minishlab/potion-code-16M-v2` (a 16M-parameter
//! static model, MIT). It is too large to ship in the crate (the crates.io limit
//! is 10 MB) and too useful to leave optional, so the first command that needs
//! embeddings downloads all of its files in one go into the data directory and
//! every later run uses the local copy. Nothing is sent to the network except
//! the three file requests.
//!
//! - The revision and the SHA-256 of every file are pinned, so a download is
//!   verified byte for byte and cannot silently change.
//! - Several processes may race: the download takes a lock and the others wait.
//! - A failed download is remembered for an hour, so working offline costs one
//!   failed attempt, not one per command.
//!
//! Environment:
//! - `CORA_EMBED_MODEL_DIR`: use a model that is already on disk (air-gapped
//!   machines, mirrors, tests); never downloads into it.
//! - `CORA_EMBED_MODEL_URL`: base URL to fetch the files from instead of the
//!   Hugging Face hub (an internal mirror); the pinned hashes still apply.
//! - `CORA_EMBED_OFFLINE=1`: never touch the network.

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Directory name and provider tag of the model.
pub const MODEL_NAME: &str = "potion-code-16M-v2";

/// Pinned revision of `minishlab/potion-code-16M-v2`.
#[cfg(test)]
const REVISION: &str = "e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b";

/// Where the files are fetched from by default.
const DEFAULT_BASE_URL: &str = "https://huggingface.co/minishlab/potion-code-16M-v2/resolve/e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b";

/// A file of the model with its expected size and SHA-256.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// The files of `potion-code-16M-v2` at [`REVISION`].
pub const FILES: &[ModelFile] = &[
    ModelFile {
        name: "config.json",
        size: 59,
        sha256: "148e5691a6fcc553437156859701fba017a1ba5d340b170f17e0f3668fb861a7",
    },
    ModelFile {
        name: "tokenizer.json",
        size: 1_024_340,
        sha256: "107bbdcbad4bff1d299b7a4c3a2fb17c52890688b7dd0e4c9deab79d3c4f3d45",
    },
    ModelFile {
        name: "model.safetensors",
        size: 32_490_072,
        sha256: "75cf7a6c2171b230ad19b1e7d8e0b1aee86da5a02af8e7cacedd9921d227623c",
    },
];

/// How long a failed download is remembered, in seconds.
const FAILURE_BACKOFF_SECS: u64 = 3600;

/// Whether the environment forbids network access for the model.
pub(crate) fn offline() -> bool {
    std::env::var("CORA_EMBED_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Directory the model is (or will be) stored in.
pub fn model_dir() -> PathBuf {
    match std::env::var_os("CORA_EMBED_MODEL_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => models_root().join(MODEL_NAME),
    }
}

fn models_root() -> PathBuf {
    crate::data_dir::cora_data_dir().join("models")
}

/// True when every file of `files` is in `dir` with its exact size. The hashes
/// are checked once, when the files are downloaded.
pub fn is_installed_in(dir: &Path, files: &[ModelFile]) -> bool {
    files.iter().all(|f| {
        std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size)
    })
}

/// Make the model available and return its directory.
///
/// With `allow_download` and no `CORA_EMBED_OFFLINE`, a missing model is
/// downloaded (all files, verified). Without it, only an installed model is
/// returned. Test builds never download.
pub fn ensure(allow_download: bool) -> Result<PathBuf> {
    let dir = model_dir();
    // A directory named by the user is theirs: any Model2Vec model with the
    // three files is accepted (its dimensions are read from the model), and
    // nothing is ever downloaded into it.
    if std::env::var_os("CORA_EMBED_MODEL_DIR").is_some_and(|d| !d.is_empty()) {
        let missing: Vec<&str> = FILES
            .iter()
            .map(|f| f.name)
            .filter(|n| !dir.join(n).is_file())
            .collect();
        if missing.is_empty() {
            return Ok(dir);
        }
        bail!(
            "CORA_EMBED_MODEL_DIR={} is missing {}",
            dir.display(),
            missing.join(", ")
        );
    }
    if is_installed_in(&dir, FILES) {
        return Ok(dir);
    }
    if !allow_download || cfg!(test) {
        bail!("the {MODEL_NAME} model is not downloaded yet");
    }
    if offline() {
        bail!("the {MODEL_NAME} model is not downloaded and CORA_EMBED_OFFLINE is set");
    }
    let root = models_root();
    if let Some(age) = recent_failure(&root) {
        bail!(
            "the last download of {MODEL_NAME} failed {} minutes ago; not retrying yet \
             (delete {} to retry now)",
            age / 60,
            failure_marker(&root).display()
        );
    }
    let base = std::env::var("CORA_EMBED_MODEL_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    match download_to(&dir, &base, FILES) {
        Ok(()) => {
            let _ = std::fs::remove_file(failure_marker(&root));
            Ok(dir)
        }
        Err(e) => {
            record_failure(&root);
            Err(e)
        }
    }
}

fn failure_marker(root: &Path) -> PathBuf {
    root.join(".last-download-failure")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Seconds since the last failed download, if it is still within the backoff.
fn recent_failure(root: &Path) -> Option<u64> {
    let at: u64 = std::fs::read_to_string(failure_marker(root))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let age = now_secs().saturating_sub(at);
    (age < FAILURE_BACKOFF_SECS).then_some(age)
}

fn record_failure(root: &Path) {
    let _ = std::fs::create_dir_all(root);
    let _ = std::fs::write(failure_marker(root), now_secs().to_string());
}

/// Download every file of `files` from `base_url` into `dir`: to a temporary
/// directory first, each file checked against its size and SHA-256, then moved
/// into place in one rename. Takes a lock so concurrent runs do not duplicate
/// the work, and returns early if another run finished it.
pub fn download_to(dir: &Path, base_url: &str, files: &[ModelFile]) -> Result<()> {
    let parent = dir.parent().context("model directory has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    let lock_path = parent.join(".download.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    fs2::FileExt::lock_exclusive(&lock).context("lock the model download")?;

    // Another process may have finished while this one waited for the lock.
    if is_installed_in(dir, files) {
        return Ok(());
    }

    let total: u64 = files.iter().map(|f| f.size).sum();
    eprintln!(
        "Downloading the code embedding model {MODEL_NAME} ({:.1} MB, once) ...",
        total as f64 / 1e6
    );

    let tmp = parent.join(format!(".download-{}-{}", std::process::id(), now_secs()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let result = run_blocking(fetch_all(&tmp, base_url, files));
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e.context(format!("download the {MODEL_NAME} model from {base_url}")));
    }
    // Replace a partial or outdated directory, then move the new one in.
    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&tmp, dir).with_context(|| format!("install {}", dir.display()))?;
    eprintln!("Model ready in {}", dir.display());
    Ok(())
}

/// Run a future to completion on a dedicated thread with its own runtime, so
/// it works from sync code and from code that is already inside a runtime.
fn run_blocking<T: Send>(fut: impl std::future::Future<Output = Result<T>> + Send) -> Result<T> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("start the download runtime")?
                .block_on(fut)
        })
        .join()
        .map_err(|_| anyhow!("the download thread panicked"))?
    })
}

async fn fetch_all(dir: &Path, base_url: &str, files: &[ModelFile]) -> Result<()> {
    use futures_util::StreamExt;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .user_agent(concat!("cora-code/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build the HTTP client")?;
    let base = base_url.trim_end_matches('/');

    for f in files {
        let url = format!("{base}/{}", f.name);
        let resp = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("request {}", f.name))?;
        if !resp.status().is_success() {
            bail!("{} answered HTTP {}", f.name, resp.status());
        }
        let path = dir.join(f.name);
        let mut out = std::fs::File::create(&path)?;
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("read {}", f.name))?;
            written += chunk.len() as u64;
            if written > f.size {
                bail!("{} is larger than the expected {} bytes", f.name, f.size);
            }
            hasher.update(&chunk);
            out.write_all(&chunk)?;
        }
        out.flush()?;
        if written != f.size {
            bail!("{} is {written} bytes, expected {}", f.name, f.size);
        }
        let digest = format!("{:x}", hasher.finalize());
        if digest != f.sha256 {
            bail!("{} failed its checksum (got {digest})", f.name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    /// A tiny HTTP server answering GET /<name> from a map, for `n` requests.
    fn serve(files: Vec<(&'static str, Vec<u8>)>, requests: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .trim_start_matches('/')
                    .to_string();
                match files.iter().find(|(name, _)| *name == path) {
                    Some((_, body)) => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = s.write_all(head.as_bytes());
                        let _ = s.write_all(body);
                    }
                    None => {
                        let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            }
        });
        format!("http://{addr}")
    }

    fn manifest(a: &[u8], b: &[u8]) -> Vec<ModelFile> {
        // leak the digests: ModelFile holds &'static str, fine in a test
        let sa: &'static str = Box::leak(sha(a).into_boxed_str());
        let sb: &'static str = Box::leak(sha(b).into_boxed_str());
        vec![
            ModelFile {
                name: "a.json",
                size: a.len() as u64,
                sha256: sa,
            },
            ModelFile {
                name: "b.bin",
                size: b.len() as u64,
                sha256: sb,
            },
        ]
    }

    #[test]
    fn a_download_is_verified_and_installed_in_one_step() {
        let (a, b) = (b"{\"x\":1}".to_vec(), vec![7u8; 5000]);
        let files = manifest(&a, &b);
        let url = serve(vec![("a.json", a.clone()), ("b.bin", b.clone())], 2);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("models").join("m");

        download_to(&dir, &url, &files).unwrap();

        assert!(is_installed_in(&dir, &files));
        assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), b);
        // no temporary directory is left behind
        let leftovers: Vec<_> = std::fs::read_dir(dir.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".download-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn a_wrong_checksum_installs_nothing() {
        let (a, b) = (b"{\"x\":1}".to_vec(), vec![7u8; 5000]);
        let files = manifest(&a, &b);
        let mut tampered = b.clone();
        tampered[0] = 8; // same size, different bytes
        let url = serve(vec![("a.json", a), ("b.bin", tampered)], 2);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("models").join("m");

        let err = download_to(&dir, &url, &files).unwrap_err();

        assert!(format!("{err:#}").contains("checksum"), "{err:#}");
        assert!(
            !dir.exists(),
            "a failed download must not leave a model behind"
        );
    }

    #[test]
    fn a_wrong_size_and_a_missing_file_fail() {
        let (a, b) = (b"{\"x\":1}".to_vec(), vec![7u8; 5000]);
        let files = manifest(&a, &b);
        let url = serve(vec![("a.json", a.clone()), ("b.bin", vec![7u8; 10])], 2);
        let tmp = tempfile::tempdir().unwrap();
        let err = download_to(&tmp.path().join("m1").join("m"), &url, &files).unwrap_err();
        assert!(format!("{err:#}").contains("bytes"), "{err:#}");

        let url = serve(vec![("a.json", a)], 2);
        let err = download_to(&tmp.path().join("m2").join("m"), &url, &files).unwrap_err();
        assert!(format!("{err:#}").contains("404"), "{err:#}");
    }

    #[test]
    fn an_installed_model_is_not_downloaded_again() {
        let (a, b) = (b"{}".to_vec(), vec![1u8; 100]);
        let files = manifest(&a, &b);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("models").join("m");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), &a).unwrap();
        std::fs::write(dir.join("b.bin"), &b).unwrap();
        // an unreachable URL: it must not be used
        download_to(&dir, "http://127.0.0.1:1", &files).unwrap();
    }

    #[test]
    fn a_partial_directory_is_not_an_installed_model() {
        let (a, b) = (b"{}".to_vec(), vec![1u8; 100]);
        let files = manifest(&a, &b);
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.json"), &a).unwrap();
        assert!(!is_installed_in(tmp.path(), &files));
        std::fs::write(tmp.path().join("b.bin"), vec![1u8; 99]).unwrap();
        assert!(!is_installed_in(tmp.path(), &files), "wrong size");
    }

    #[test]
    fn the_pinned_manifest_lists_the_three_model_files() {
        let names: Vec<&str> = FILES.iter().map(|f| f.name).collect();
        assert_eq!(
            names,
            ["config.json", "tokenizer.json", "model.safetensors"]
        );
        assert!(FILES.iter().all(|f| f.sha256.len() == 64 && f.size > 0));
        assert!(DEFAULT_BASE_URL.contains(REVISION));
    }

    #[test]
    fn a_recent_failure_blocks_retries_and_an_old_one_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(recent_failure(tmp.path()).is_none());
        record_failure(tmp.path());
        assert!(recent_failure(tmp.path()).is_some());
        std::fs::write(
            failure_marker(tmp.path()),
            (now_secs() - FAILURE_BACKOFF_SECS - 5).to_string(),
        )
        .unwrap();
        assert!(recent_failure(tmp.path()).is_none());
    }
}
