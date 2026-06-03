use anyhow::Result;
use colored::Colorize;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// `tokenix doctor` — diagnose the embedding backend, GPU availability, model
/// cache, and daemon, then print tailored recommendations. Read-only: it never
/// installs anything or starts the daemon.
pub fn run_doctor() -> Result<()> {
    println!();
    println!("{}", "tokenix doctor".bold());
    println!("{}", "  environment & acceleration diagnostics".dimmed());
    println!();

    section("Build");
    kv("version", env!("CARGO_PKG_VERSION"));
    match crate::embed::gpu_backend() {
        Some(b) => kv(
            "gpu support compiled",
            &format!("{b} (GPU used by default)"),
        ),
        None => kv("gpu support compiled", "none — CPU-only build"),
    }
    println!();

    section("GPU");
    let nvidia = detect_nvidia();
    match &nvidia {
        Some((name, driver)) => kv("nvidia gpu", &format!("{name} (driver {driver})")),
        None => kv("nvidia gpu", "not detected (or nvidia-smi unavailable)"),
    }
    let cuda = detect_cuda_toolkit();
    kv(
        "cuda toolkit",
        &cuda.clone().unwrap_or_else(|| "not found".to_string()),
    );
    kv("cudnn", if detect_cudnn() { "found" } else { "not found" });
    println!();

    section("Embedding model");
    let model_dir = crate::embed::model_cache_dir();
    match dir_size(&model_dir) {
        Some(bytes) if bytes > 0 => kv(
            "cache",
            &format!(
                "ready ({:.0} MB at {})",
                bytes as f64 / 1e6,
                model_dir.display()
            ),
        ),
        _ => kv(
            "cache",
            "not downloaded yet — fetched (~130 MB) on first embed",
        ),
    }
    println!();

    section("Daemon");
    kv(
        "status",
        if daemon_running() {
            "running"
        } else {
            "not running (auto-starts on first Grep hook, or `tokenix serve`)"
        },
    );
    println!();

    section("Recommendations");
    print_recommendations(&nvidia, cuda.is_some(), detect_cudnn());
    println!();
    Ok(())
}

fn print_recommendations(nvidia: &Option<(String, String)>, cuda: bool, cudnn: bool) {
    match crate::embed::gpu_backend() {
        Some(backend) => {
            tip(&format!(
                "GPU acceleration is active by default ({backend}). Force CPU with `tokenix --only-cpu ...`."
            ));
            if backend == "CUDA" && (!cuda || !cudnn) {
                warn("CUDA build but CUDA Toolkit/cuDNN not detected — it will fall back to CPU. Install CUDA 12.x + cuDNN 9.x and put them on PATH.");
            }
        }
        None => {
            if nvidia.is_some() {
                tip("CPU-only build, but an NVIDIA GPU is present. For ~10x faster indexing on Windows (no extra deps):");
                println!(
                    "      {}",
                    "cargo install --path . --features directml --locked".cyan()
                );
                tip("CUDA can be ~2-3x faster than DirectML but needs CUDA 12.x + cuDNN 9.x installed:");
                println!(
                    "      {}",
                    "cargo install --path . --features cuda --locked".cyan()
                );
            } else {
                tip("CPU-only build. Embedding runs on CPU; that is fine for small/medium repos. A supported GPU enables `--features directml`/`cuda`.");
            }
        }
    }
}

// ---- detection helpers ------------------------------------------------------

fn detect_nvidia() -> Option<(String, String)> {
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=name,driver_version", "--format=csv,noheader"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let first = line.lines().next()?.trim();
    let (name, driver) = first.split_once(',')?;
    Some((name.trim().to_string(), driver.trim().to_string()))
}

fn detect_cuda_toolkit() -> Option<String> {
    if let Ok(out) = Command::new("nvcc").arg("--version").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(rel) = s.lines().find(|l| l.contains("release")) {
                return Some(rel.trim().to_string());
            }
            return Some("found".to_string());
        }
    }
    std::env::var("CUDA_PATH")
        .ok()
        .map(|p| format!("CUDA_PATH={p}"))
}

fn detect_cudnn() -> bool {
    let mut roots: Vec<PathBuf> = Vec::new();

    #[cfg(windows)]
    {
        // cudnn64_*.dll lives in %CUDA_PATH%\bin on Windows.
        if let Ok(p) = std::env::var("CUDA_PATH") {
            roots.push(PathBuf::from(&p).join("bin"));
            roots.push(PathBuf::from(p));
        }
    }

    #[cfg(unix)]
    {
        // libcudnn.so.* on standard Linux paths and common CUDA install locations.
        roots.push(PathBuf::from("/usr/lib"));
        roots.push(PathBuf::from("/usr/lib/x86_64-linux-gnu"));
        roots.push(PathBuf::from("/usr/local/cuda/lib64"));
        roots.push(PathBuf::from("/usr/local/cuda/lib"));
        if let Ok(p) = std::env::var("CUDA_PATH") {
            roots.push(PathBuf::from(p).join("lib64"));
        }
    }

    for root in roots {
        if let Ok(rd) = std::fs::read_dir(&root) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                let is_cudnn = n.contains("cudnn");
                #[cfg(windows)]
                let is_lib = n.ends_with(".dll");
                #[cfg(unix)]
                let is_lib = n.contains(".so");
                if is_cudnn && is_lib {
                    return true;
                }
            }
        }
    }
    false
}

fn daemon_running() -> bool {
    let port = crate::daemon::daemon_port();
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(200),
    )
    .is_ok()
}

fn dir_size(dir: &std::path::Path) -> Option<u64> {
    let mut total = 0u64;
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        if let Ok(meta) = e.metadata() {
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                total += dir_size(&e.path()).unwrap_or(0);
            }
        }
    }
    Some(total)
}

// ---- formatting -------------------------------------------------------------

fn section(name: &str) {
    println!("{}", name.bold());
}

fn kv(key: &str, value: &str) {
    println!("  {:<22} {}", format!("{key}:").dimmed(), value);
}

fn tip(msg: &str) {
    println!("  {} {}", "•".green(), msg);
}

fn warn(msg: &str) {
    println!("  {} {}", "!".yellow(), msg.yellow());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_temp_dir(sub: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir()
            .join("tokenix_test_doctor")
            .join(format!("{}_{}", sub, std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p
    }

    #[test]
    fn test_dir_size() {
        let temp_dir = create_test_temp_dir("dir_size");
        let file_path = temp_dir.join("test_file.txt");
        std::fs::write(&file_path, "hello world").unwrap(); // 11 bytes

        let size = dir_size(&temp_dir).unwrap();
        assert_eq!(size, 11);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_formatting_no_panic() {
        // Just verify formatting calls don't panic
        section("Test Section");
        kv("Key", "Value");
        tip("Tip message");
        warn("Warn message");
    }
}
