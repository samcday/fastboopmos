use notify::{RecursiveMode, Watcher};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEFAULT_ADDR: &str = "127.0.0.1:8080";
const WASM_PATH: &str = "infra/frontdoor/target/wasm32-wasip2/debug/frontdoor_edge.wasm";

pub fn run() {
    let edge_artifact_id = fs::read_to_string("infra/k8s/fastboopmos/latest.txt")
        .expect("failed to read infra/k8s/fastboopmos/latest.txt")
        .trim()
        .to_string();
    eprintln!("edge artifact id: {edge_artifact_id}");

    let cache_dir = std::env::temp_dir().join("fastboopmos-cache");
    fs::create_dir_all(&cache_dir).expect("failed to create cache dir");
    eprintln!("cache dir: {}", cache_dir.display());

    if !build() {
        eprintln!("initial build failed");
        std::process::exit(1);
    }

    let mut child = serve(
        &edge_artifact_id,
        &cache_dir.to_string_lossy(),
        DEFAULT_ADDR,
    );

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && (event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove())
        {
            let _ = tx.send(());
        }
    })
    .expect("failed to create file watcher");

    watcher
        .watch(
            Path::new("infra/frontdoor/crates"),
            RecursiveMode::Recursive,
        )
        .expect("failed to watch infra/frontdoor/crates");
    watcher
        .watch(
            Path::new("infra/frontdoor/Cargo.toml"),
            RecursiveMode::NonRecursive,
        )
        .expect("failed to watch infra/frontdoor/Cargo.toml");

    eprintln!("watching infra/frontdoor/crates/ for changes...");

    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }

        let debounce = Duration::from_millis(300);
        let mut last_event = Instant::now();
        loop {
            match rx.recv_timeout(debounce) {
                Ok(()) => last_event = Instant::now(),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if last_event.elapsed() >= debounce {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        eprintln!("\n--- change detected, rebuilding... ---\n");

        if build() {
            eprintln!("build succeeded, restarting wasmtime...");
            let _ = child.kill();
            let _ = child.wait();
            child = serve(
                &edge_artifact_id,
                &cache_dir.to_string_lossy(),
                DEFAULT_ADDR,
            );
        } else {
            eprintln!("build failed, keeping previous version running");
        }
    }

    let _ = child.kill();
    let _ = child.wait();
}

fn build() -> bool {
    eprintln!("building frontdoor-edge for wasm32-wasip2...");
    let status = Command::new("cargo")
        .current_dir("infra/frontdoor")
        .env_remove("RUSTUP_TOOLCHAIN")
        .args(["build", "--target", "wasm32-wasip2", "-p", "frontdoor-edge"])
        .status()
        .expect("failed to run cargo build");
    status.success()
}

fn serve(edge_artifact_id: &str, cache_dir: &str, addr: &str) -> Child {
    let dir_arg = format!("{cache_dir}::/cache");
    eprintln!("starting wasmtime serve on http://{addr}/");

    let mut args = vec![
        "serve".to_string(),
        "--wasi".to_string(),
        "cli".to_string(),
        "--wasi".to_string(),
        "http".to_string(),
        "--addr".to_string(),
        addr.to_string(),
        "--env".to_string(),
        format!(
            "RUST_LOG={}",
            std::env::var("RUST_LOG").as_deref().unwrap_or("info")
        ),
        "--env".to_string(),
        format!("EDGE_CHANNEL_ARTIFACT_ID={edge_artifact_id}"),
        "--env".to_string(),
        "GITHUB_OWNER=samcday".to_string(),
        "--env".to_string(),
        "GITHUB_REPO=fastboopmos".to_string(),
        "--env".to_string(),
        "ASSET_NAME=edge.channel".to_string(),
        "--env".to_string(),
        "SHA256_ASSET_NAME=edge.channel.sha256".to_string(),
        "--dir".to_string(),
        dir_arg,
    ];

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        args.push("--env".to_string());
        args.push(format!("GITHUB_TOKEN={token}"));
    }

    args.push(WASM_PATH.to_string());

    let mut child = Command::new("wasmtime")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start wasmtime serve");

    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("{line}");
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("{line}");
            }
        });
    }

    child
}
