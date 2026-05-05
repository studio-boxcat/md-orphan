// Standalone walk benchmark — compares variants of the walk strategy.
// cargo run --release --example walk_bench -- /path/to/repo
use md_orphan::discovery::index_repo;
use md_orphan::exclude::{ExcludeMatcher, DEFAULT_EXCLUDES};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

fn time_fn<F: FnMut()>(mut f: F, runs: usize) -> (Duration, Duration) {
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        f();
        times.push(t.elapsed());
    }
    times.sort();
    (times[0], times[runs / 2])
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = args.get(1).map(|s| s.as_str()).unwrap_or(".");
    let runs = 5;

    // Variant A: current production index_repo (sorted, full bookkeeping).
    let a = time_fn(|| {
        let _ = index_repo(root, &[], true, false);
    }, runs);
    eprintln!("A index_repo (sorted):           min={:?} median={:?}", a.0, a.1);

    // Variant B: walkdir, no sort, same filter_entry, no by_name population.
    let matcher = ExcludeMatcher::new(DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
    let b = time_fn(|| {
        let mut count = 0usize;
        let walker = WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| {
                if !e.file_type().is_dir() { return true; }
                if e.depth() == 0 { return true; }
                let n = e.file_name().to_string_lossy();
                let n: &str = &n;
                if n.starts_with('.') && n.len() > 1 { return false; }
                if matcher.matches_bare(n) { return false; }
                true
            });
        for entry in walker.flatten() {
            if entry.file_type().is_file() { count += 1; }
        }
        std::hint::black_box(count);
    }, runs);
    eprintln!("B walkdir bare (no sort, count): min={:?} median={:?}", b.0, b.1);

    // Variant C: walkdir, sorted (sort_by_file_name only).
    let c = time_fn(|| {
        let mut count = 0usize;
        let walker = WalkDir::new(root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                if !e.file_type().is_dir() { return true; }
                if e.depth() == 0 { return true; }
                let n = e.file_name().to_string_lossy();
                let n: &str = &n;
                if n.starts_with('.') && n.len() > 1 { return false; }
                if matcher.matches_bare(n) { return false; }
                true
            });
        for entry in walker.flatten() {
            if entry.file_type().is_file() { count += 1; }
        }
        std::hint::black_box(count);
    }, runs);
    eprintln!("C walkdir bare (sorted, count):  min={:?} median={:?}", c.0, c.1);

    // Variant D: raw fs::read_dir recursion, no sort, no allocation beyond required.
    fn raw_walk(path: &std::path::Path, matcher: &ExcludeMatcher, count: &mut usize) {
        let rd = match std::fs::read_dir(path) { Ok(rd) => rd, Err(_) => return };
        for ent in rd.flatten() {
            let ft = match ent.file_type() { Ok(t) => t, Err(_) => continue };
            if ft.is_dir() {
                let name = ent.file_name();
                let n = name.to_string_lossy();
                let n: &str = &n;
                if n.starts_with('.') && n.len() > 1 { continue; }
                if matcher.matches_bare(n) { continue; }
                raw_walk(&ent.path(), matcher, count);
            } else if ft.is_file() {
                *count += 1;
            }
        }
    }
    let d = time_fn(|| {
        let mut count = 0usize;
        raw_walk(std::path::Path::new(root), &matcher, &mut count);
        std::hint::black_box(count);
    }, runs);
    eprintln!("D raw fs::read_dir recursion:    min={:?} median={:?}", d.0, d.1);
}
