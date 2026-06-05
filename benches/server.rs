//! Server hot-path benchmark: measures the backend work behind the slowest
//! `gargo --server` endpoints, isolated from HTTP/transport so we can optimize
//! the actual cost centers.
//!
//! A HAR captured from a real browsing session showed nearly all request time
//! is server-side `wait` (not transfer), dominated by:
//! - `/api/files` ~120ms each, called repeatedly, never cached
//!   -> `project::collect_files` (spawns `git ls-files` twice)
//! - `/blob/...` ~217ms, `/api/highlight` ~55ms
//!   -> `syntax::highlight::highlight_text` (tree-sitter)
//!
//! This bench exercises those two functions against THIS repo so numbers are
//! reproducible and reflect a realistic working tree.
//!
//! A later HAR (gargo_v2.har) showed the `/status` and `/branches` pages were
//! dominated by *serial* git subprocess spawns: the HTML handlers awaited 4
//! `git` processes one-at-a-time (one redundantly re-fetching the remote URL),
//! and `/api/status` awaited `git diff` then `git diff --cached` in sequence.
//! The `status git spawns` section below measures that serial-vs-concurrent
//! gap directly, since the handlers themselves are `pub(crate)` and unreachable
//! from a bench crate.
//!
//! Run: cargo run --bench bench-server --release
//!  (or: cargo bench --bench bench-server)

#[path = "common.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::time::Instant;

use gargo::project::collect_files;
use gargo::syntax::highlight::highlight_text;
use gargo::syntax::language::LanguageRegistry;

use common::{format_us, stat_avg, stat_percentile};

/// The gargo repo root (where this bench is compiled from), used as a real
/// git working tree for the `collect_files` benchmark.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ---------------------------------------------------------------------------
// Benchmark: collect_files  (/api/files hot path)
// ---------------------------------------------------------------------------

/// Each call spawns `git ls-files --cached --others --exclude-standard` AND
/// `git ls-files --deleted`, then filters. No caching today: the editor calls
/// `/api/files` on every Cmd+P open, so this runs in full each time.
fn bench_collect_files(root: &Path, warmup: usize, iterations: usize) -> (Vec<f64>, usize) {
    let mut count = 0;
    for _ in 0..warmup {
        count = collect_files(root).len();
    }

    let mut times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        let files = collect_files(root);
        times.push(t0.elapsed().as_secs_f64() * 1_000_000.0);
        count = files.len();
    }
    (times, count)
}

// ---------------------------------------------------------------------------
// Benchmark: highlight_text  (/api/highlight, /blob render)
// ---------------------------------------------------------------------------

/// `highlight_text` memoizes by (text, language), so a fixed input hits cache
/// after the first call. The editor highlights *different* content on every
/// request, so the realistic cost is the cache-MISS path: build a tree-sitter
/// `Parser`, compile the `Query`, parse, and walk captures. We force a miss
/// each iteration by prepending a unique comment line.
fn bench_highlight_miss(
    source: &str,
    lang: &gargo::syntax::language::LanguageDef,
    warmup: usize,
    iterations: usize,
    mut salt: usize,
) -> Vec<f64> {
    for _ in 0..warmup {
        let text = format!("// warm {salt}\n{source}");
        let _ = highlight_text(&text, lang);
        salt += 1;
    }

    let mut times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let text = format!("// iter {salt}\n{source}");
        salt += 1;
        let t0 = Instant::now();
        let spans = highlight_text(&text, lang);
        times.push(t0.elapsed().as_secs_f64() * 1_000_000.0);
        std::hint::black_box(&spans);
    }
    times
}

/// Steady-state (cache-HIT) cost for the same input: what a repeat view of an
/// unchanged file costs. Shows the headroom a cache buys vs. the miss path.
fn bench_highlight_hit(
    source: &str,
    lang: &gargo::syntax::language::LanguageDef,
    warmup: usize,
    iterations: usize,
) -> Vec<f64> {
    // Prime the cache once.
    let _ = highlight_text(source, lang);
    for _ in 0..warmup {
        let _ = highlight_text(source, lang);
    }

    let mut times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        let spans = highlight_text(source, lang);
        times.push(t0.elapsed().as_secs_f64() * 1_000_000.0);
        std::hint::black_box(&spans);
    }
    times
}

// ---------------------------------------------------------------------------
// Benchmark: status / page-context git spawns  (/status, /branches, /api/status)
// ---------------------------------------------------------------------------

/// Mirror of the server's `git_output_in_repo`: same `-c` flags so spawn cost
/// matches the real handlers.
async fn git_out(root: &Path, args: &[&str]) -> String {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["-c", "core.quotepath=off"]);
    cmd.args(["-c", "core.optionalLocks=false"]);
    cmd.args(args);
    cmd.current_dir(root);
    match cmd.output().await {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

/// `/api/status` git work, the OLD way: two diffs awaited back-to-back.
async fn status_diffs_serial(root: &Path) {
    let _unstaged = git_out(root, &["diff"]).await;
    let _staged = git_out(root, &["diff", "--cached"]).await;
}

/// `/api/status` git work, the NEW way: both diffs spawned concurrently.
async fn status_diffs_parallel(root: &Path) {
    let _ = tokio::join!(
        git_out(root, &["diff"]),
        git_out(root, &["diff", "--cached"]),
    );
}

/// `/status` / `/branches` header context, the OLD way: 4 serial spawns, the
/// 3rd a redundant re-fetch of the remote URL.
async fn page_context_serial(root: &Path) {
    let _remote = git_out(root, &["config", "--get", "remote.origin.url"]).await;
    let _branch = git_out(root, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    let _remote_again = git_out(root, &["config", "--get", "remote.origin.url"]).await;
    let _default = git_out(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await;
}

/// `/status` / `/branches` header context, the NEW way: dedup the remote URL and
/// spawn the 3 distinct lookups concurrently. (Caching makes steady-state state
/// even cheaper — this measures the cold, uncached path.)
async fn page_context_parallel(root: &Path) {
    let _ = tokio::join!(
        git_out(root, &["config", "--get", "remote.origin.url"]),
        git_out(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        git_out(
            root,
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]
        ),
    );
}

/// Time an async closure over warmup + iterations, returning per-iter micros.
fn time_async<F, Fut>(
    rt: &tokio::runtime::Runtime,
    warmup: usize,
    iters: usize,
    mut f: F,
) -> Vec<f64>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for _ in 0..warmup {
        rt.block_on(f());
    }
    let mut times = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t0 = Instant::now();
        rt.block_on(f());
        times.push(t0.elapsed().as_secs_f64() * 1_000_000.0);
    }
    times
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let warmup = 5;
    let iterations = 50;
    let root = repo_root();

    println!("Gargo Benchmark: Server hot paths (release build)");
    println!("Repo: {}", root.display());
    println!("Iterations: {iterations}, Warmup: {warmup}");

    // -----------------------------------------------------------------------
    // 1. collect_files  (/api/files)
    // -----------------------------------------------------------------------
    println!();
    println!("=== collect_files: /api/files hot path (git ls-files x2) ===");
    println!("{:>8} {:>10} {:>10} {:>10}", "files", "avg", "p95", "p99");

    let (mut times, n) = bench_collect_files(&root, warmup, iterations);
    let avg = format_us(stat_avg(&times));
    let p95 = format_us(stat_percentile(&mut times, 95.0));
    let p99 = format_us(stat_percentile(&mut times, 99.0));
    println!("{n:>8} {avg:>10} {p95:>10} {p99:>10}");

    // -----------------------------------------------------------------------
    // 2. highlight_text  (/api/highlight, /blob render)
    // -----------------------------------------------------------------------
    println!();
    println!("=== highlight_text: tree-sitter highlight per request ===");
    println!(
        "{:>22} {:>6} {:>8} {:>10} {:>10} {:>10}",
        "file", "lines", "mode", "avg", "p95", "p99"
    );

    let registry = LanguageRegistry::new();
    // A handful of real source files of varying size, so the numbers map onto
    // what an editor session actually highlights.
    let samples = [
        "src/syntax/highlight.rs",
        "src/command/diff_server.rs",
        "README.md",
        "src/command/web_editor_server.rs",
    ];

    for (i, rel) in samples.iter().enumerate() {
        let path = root.join(rel);
        let Ok(source) = std::fs::read_to_string(&path) else {
            println!("{rel:>22}  (skipped: unreadable)");
            continue;
        };
        let Some(lang) = registry.detect_by_extension(rel) else {
            println!("{rel:>22}  (skipped: no language)");
            continue;
        };
        let lines = source.lines().count();

        let mut miss = bench_highlight_miss(&source, lang, warmup, iterations, i * 100_000);
        let m_avg = format_us(stat_avg(&miss));
        let m_p95 = format_us(stat_percentile(&mut miss, 95.0));
        let m_p99 = format_us(stat_percentile(&mut miss, 99.0));
        println!(
            "{:>22} {:>6} {:>8} {:>10} {:>10} {:>10}",
            rel, lines, "miss", m_avg, m_p95, m_p99
        );

        let mut hit = bench_highlight_hit(&source, lang, warmup, iterations);
        let h_avg = format_us(stat_avg(&hit));
        let h_p95 = format_us(stat_percentile(&mut hit, 95.0));
        let h_p99 = format_us(stat_percentile(&mut hit, 99.0));
        println!(
            "{:>22} {:>6} {:>8} {:>10} {:>10} {:>10}",
            "", "", "hit", h_avg, h_p95, h_p99
        );
    }

    // -----------------------------------------------------------------------
    // 3. status / page-context git spawns  (/status, /branches, /api/status)
    // -----------------------------------------------------------------------
    println!();
    println!("=== git-spawn pattern: serial (old) vs concurrent (new) ===");
    println!("{:>26} {:>10} {:>10} {:>10}", "case", "avg", "p95", "p99");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let report = |label: &str, times: &mut Vec<f64>| {
        let avg = format_us(stat_avg(times));
        let p95 = format_us(stat_percentile(times, 95.0));
        let p99 = format_us(stat_percentile(times, 99.0));
        println!("{label:>26} {avg:>10} {p95:>10} {p99:>10}");
    };

    let mut t = time_async(&rt, warmup, iterations, || status_diffs_serial(&root));
    report("api/status diffs: serial", &mut t);
    let mut t = time_async(&rt, warmup, iterations, || status_diffs_parallel(&root));
    report("api/status diffs: parallel", &mut t);
    let mut t = time_async(&rt, warmup, iterations, || page_context_serial(&root));
    report("page ctx (4 spawns): serial", &mut t);
    let mut t = time_async(&rt, warmup, iterations, || page_context_parallel(&root));
    report("page ctx (dedup): parallel", &mut t);
}
