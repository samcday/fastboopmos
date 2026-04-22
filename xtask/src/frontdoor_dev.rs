use notify::{RecursiveMode, Watcher};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

    let mut child = serve(&edge_artifact_id, &cache_dir.to_string_lossy());

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
            Path::new("infra/frontdoor/src"),
            RecursiveMode::Recursive,
        )
        .expect("failed to watch infra/frontdoor/src");

    eprintln!("watching infra/frontdoor/src/ for changes...");

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
            eprintln!("build succeeded, restarting frontdoor...");
            let _ = child.kill();
            let _ = child.wait();
            child = serve(&edge_artifact_id, &cache_dir.to_string_lossy());
        } else {
            eprintln!("build failed, keeping previous version running");
        }
    }

    let _ = child.kill();
    let _ = child.wait();
}

fn build() -> bool {
    eprintln!("building frontdoor...");
    let status = Command::new("cargo")
        .current_dir("infra/frontdoor")
        .env_remove("RUSTUP_TOOLCHAIN")
        .args(["build"])
        .status()
        .expect("failed to run cargo build");
    status.success()
}

fn serve(edge_artifact_id: &str, cache_dir: &str) -> Child {
    let bin_path = "infra/frontdoor/target/debug/frontdoor";
    eprintln!("starting frontdoor on http://127.0.0.1:8080/");

    let mut cmd = Command::new(bin_path);
    cmd.env("PORT", "8080")
        .env("CACHE_DIR", cache_dir)
        .env("EDGE_CHANNEL_ARTIFACT_ID", edge_artifact_id)
        .env("GITHUB_OWNER", "samcday")
        .env("GITHUB_REPO", "fastboopmos");

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        cmd.env("GITHUB_TOKEN", &token);
    }

    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        cmd.env("RUST_LOG", &rust_log);
    }

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start frontdoor");

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
