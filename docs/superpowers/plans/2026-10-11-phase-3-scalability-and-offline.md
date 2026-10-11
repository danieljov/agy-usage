# Phase 3: Long-Term Scalability & Offline Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement Phase 3 long-term scalability and offline resilience by adding a sliding retention window with sidecar-locked atomic pruning, incremental store loading for the TUI, lazy prune triggers on collection, and vendored offline Chart.js bundles.

**Architecture:** A sidecar lock file (`samples.lock`) coordinates exclusive access during writes and atomic file rewrites (`prune`), while shared locks allow concurrent readers. The TUI tracks an inode-aware cursor `{ino, offset}` to load only newly appended records $O(\Delta)$, with automatic reset when the file is replaced (prune rename) or truncated. `assets/vendor/` embeds pinned, minified Chart.js and date adapter directly into `dashboard.html` for zero-network offline rendering.

**Tech Stack:** Rust 2024, `rustix::fs::flock`, `tempfile::NamedTempFile`, `serde_json`, `chrono`, `clap`, `ratatui`. No new dependencies (CLI tests keep using `std::process::Command` + `env!("CARGO_BIN_EXE_agy-usage")`).

**Spec:** [`docs/superpowers/specs/2026-10-11-phase-3-scalability-and-offline-design.md`](file:///home/daniel/projects/ai_resources/docs/superpowers/specs/2026-10-11-phase-3-scalability-and-offline-design.md)

## Global Constraints

- Never break or change the sample JSONL format (`ts`, `group`, `bucket_id`, `window`, `remaining`, `reset_time`).
- All storage file mutations must hold `FlockOperation::LockExclusive` on `samples.lock` (derived via `path.with_extension("lock")`). **Lock ordering is mandatory:** acquire the sidecar lock *before* opening `samples.jsonl`, and release it only after the write/rename completes.
- `store::prune` must write kept samples to a temporary file in `path.parent()` and atomically rename over `samples.jsonl` while holding the sidecar lock for the entire read → write → rename sequence.
- `--retention-days` is `Option<u32>` (range `8..=36500`). Effective retention = explicit value, or `max(90, --history-days)` when omitted.
- If `--retention-days` is **explicitly** given and is `< --history-days`, the CLI must error and exit non-zero. Omitting it must never fail (keeps already-installed timer units with `--history-days > 90` working after upgrade).
- The generated HTML must be 100% self-contained with zero external `<script src="...">` tags.
- Full verification (`cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`) must pass with zero errors and zero warnings.

## Review Focus

1. **Concurrent Append & Prune Race**: An append must never write to an unlinked inode. Because `append` takes the sidecar lock *before* opening `samples.jsonl`, it either completes before prune starts or opens the new inode after the rename.
2. **File Replacement in TUI**: `load_from` must detect both an in-place truncation (`len < offset`) and a replacement via rename (`ino` changed, even if the new file is larger than the old offset) and report `reset = true` with all surviving records.
3. **Empty / Corrupt Lines During Prune**: `prune` must skip corrupt lines, preserve every valid sample with `ts >= cutoff`, and preserve the original file permissions.
4. **Offline HTML Fallback**: The vendored Chart.js must initialize cleanly without network access, including boundary markers and tooltips. Vendored JS must not contain `</script`.
5. **Timer Directive Escaping**: The propagated `--retention-days` in `ExecStart=` must preserve systemd directive encoding without double expansion.
6. **Upgrade Compatibility**: An existing unit running `--history-days 120 collect` (no `--retention-days`) must keep succeeding.

---

### Task 1: Vendor Offline Chart.js & Date Adapter

**Files:**
- Create: `assets/vendor/chart.umd.min.js`
- Create: `assets/vendor/chartjs-adapter-date-fns.bundle.min.js`
- Create: `assets/vendor/README.md` (versions, source URLs, SHA-256)
- Create: `assets/vendor/LICENSE-chartjs` and `assets/vendor/LICENSE-chartjs-adapter-date-fns` (MIT notices)
- Modify: `assets/dashboard.html`
- Modify: `src/html.rs`

**Interfaces:**
- Consumes: `assets/vendor/*.min.js`
- Produces: `html::render(reports, generated_at, history_days) -> String` with embedded inline JavaScript.

- [ ] **Step 1: Resolve exact versions and download pinned bundles**

```bash
mkdir -p assets/vendor
CHART_VER=$(curl -fsSL 'https://data.jsdelivr.com/v1/packages/npm/chart.js/resolved?specifier=4' | sed -E 's/.*"version":"([^"]+)".*/\1/')
ADAPTER_VER=$(curl -fsSL 'https://data.jsdelivr.com/v1/packages/npm/chartjs-adapter-date-fns/resolved?specifier=3' | sed -E 's/.*"version":"([^"]+)".*/\1/')
echo "chart.js=$CHART_VER adapter=$ADAPTER_VER"
curl -fsSL "https://cdn.jsdelivr.net/npm/chart.js@${CHART_VER}/dist/chart.umd.min.js" -o assets/vendor/chart.umd.min.js
curl -fsSL "https://cdn.jsdelivr.net/npm/chartjs-adapter-date-fns@${ADAPTER_VER}/dist/chartjs-adapter-date-fns.bundle.min.js" -o assets/vendor/chartjs-adapter-date-fns.bundle.min.js
curl -fsSL "https://cdn.jsdelivr.net/npm/chart.js@${CHART_VER}/LICENSE.md" -o assets/vendor/LICENSE-chartjs
curl -fsSL "https://cdn.jsdelivr.net/npm/chartjs-adapter-date-fns@${ADAPTER_VER}/LICENSE.md" -o assets/vendor/LICENSE-chartjs-adapter-date-fns
sha256sum assets/vendor/*.min.js
```

Write `assets/vendor/README.md` recording both exact versions, source URLs, and the `sha256sum` output. Sanity check: both `.min.js` files are non-trivial in size and start with JavaScript, not an HTML error page. If a `LICENSE.md` download 404s, browse `https://cdn.jsdelivr.net/npm/<pkg>@<ver>/` for the actual license filename.

- [ ] **Step 2: Write failing tests in `src/html.rs`**

```rust
#[test]
fn renders_with_inlined_scripts_zero_external_cdns() {
    let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
    let html = render(&[], now, 30);

    assert!(!html.contains("cdn.jsdelivr.net"));
    assert!(!html.contains("<script src="));
    assert!(html.contains(CHART_JS));
    assert!(html.contains(DATE_FNS_ADAPTER));
    assert!(!html.contains("/*__CHART_JS__*/"));
    assert!(!html.contains("/*__DATE_FNS_ADAPTER__*/"));
}

#[test]
fn vendored_bundles_contain_no_script_close_tag() {
    for js in [CHART_JS, DATE_FNS_ADAPTER] {
        assert!(!js.to_ascii_lowercase().contains("</script"));
    }
}

#[test]
fn data_cannot_inject_vendor_placeholders() {
    let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
    let s = Sample {
        ts: now,
        group: "/*__CHART_JS__*/".to_string(),
        bucket_id: "/*__DATE_FNS_ADAPTER__*/".to_string(),
        window: "5h".to_string(),
        remaining: 0.8,
        reset_time: now + Duration::hours(5),
    };
    let rep = make_report("/*__CHART_JS__*/", "/*__DATE_FNS_ADAPTER__*/", "5h", vec![s]);
    let html = render(&[rep], now, 30);

    assert_eq!(html.matches(CHART_JS).count(), 1);
    assert_eq!(html.matches(DATE_FNS_ADAPTER).count(), 1);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test html::tests`
Expected: FAIL to compile (`CHART_JS` / `DATE_FNS_ADAPTER` not found).

- [ ] **Step 4: Update `assets/dashboard.html` and `src/html.rs` to inline vendored bundles**

In `assets/dashboard.html`, replace the two CDN tags (lines 112–113) with:

```html
<script>/*__CHART_JS__*/</script>
<script>/*__DATE_FNS_ADAPTER__*/</script>
```

In `src/html.rs`:

```rust
const CHART_JS: &str = include_str!("../assets/vendor/chart.umd.min.js");
const DATE_FNS_ADAPTER: &str =
    include_str!("../assets/vendor/chartjs-adapter-date-fns.bundle.min.js");
```

In `render()`, **substitute vendor placeholders first, then data**, so user-controlled data can never be expanded into a bundle:

```rust
TEMPLATE
    .replace("/*__CHART_JS__*/", CHART_JS)
    .replace("/*__DATE_FNS_ADAPTER__*/", DATE_FNS_ADAPTER)
    .replace("/*__AGY_DATA__*/null", &safe_json)
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test html::`
Expected: PASS (all 12 html tests: 9 existing + 3 new).

- [ ] **Step 6: Manual offline smoke test**

Generate `agy-usage html --out /tmp/agy-offline.html` with a populated data dir, open it with networking disabled (e.g. browser offline mode), and confirm charts, boundary markers, and tooltips render with no console errors.

- [ ] **Step 7: Commit Task 1**

```bash
git add assets/vendor assets/dashboard.html src/html.rs
git commit -m "feat(html): vendor pinned Chart.js and date adapter for offline dashboards"
```

---

### Task 2: Sidecar Lock Protocol, Store Pruning, and Oldest Timestamp

**Files:**
- Modify: `src/store.rs`

**Interfaces:**
- Produces:
  - `store::append(path: &Path, samples: &[Sample]) -> std::io::Result<()>` (uses `samples.lock`)
  - `store::load(path: &Path) -> std::io::Result<Loaded>` (uses `samples.lock`)
  - `store::oldest_timestamp(path: &Path) -> std::io::Result<Option<DateTime<Utc>>>` (first non-empty **valid** line)
  - `store::prune(path: &Path, cutoff: DateTime<Utc>) -> std::io::Result<usize>` (returns number of valid samples removed; corrupt lines are dropped and not counted)

- [ ] **Step 1: Write failing tests in `src/store.rs`**

```rust
fn sample_at(id: &str, ts: DateTime<Utc>) -> Sample {
    let mut s = make_sample(id, 0.5);
    s.ts = ts;
    s
}

#[test]
fn oldest_timestamp_skips_empty_and_corrupt_leading_lines() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");

    assert_eq!(oldest_timestamp(&path).unwrap(), None);

    let s1 = make_sample("b1", 0.9);
    let s2 = make_sample("b2", 0.8);
    let mut content: Vec<u8> = b"\n{not json}\n\xff\n".to_vec();
    content.extend_from_slice((serde_json::to_string(&s1).unwrap() + "\n").as_bytes());
    content.extend_from_slice((serde_json::to_string(&s2).unwrap() + "\n").as_bytes());
    fs::write(&path, content).unwrap();

    assert_eq!(oldest_timestamp(&path).unwrap(), Some(s1.ts));
}

#[test]
fn prune_filters_old_records_and_skips_corrupt_lines() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");
    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();

    let old = sample_at("old", Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap());
    let boundary = sample_at("boundary", cutoff); // ts == cutoff is kept
    let new = sample_at("new", Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap());

    append(&path, &[old]).unwrap();
    fs::OpenOptions::new().append(true).open(&path).unwrap()
        .write_all(b"{corrupt\n").unwrap();
    append(&path, &[boundary.clone(), new.clone()]).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    let pruned = prune(&path, cutoff).unwrap();
    assert_eq!(pruned, 1);

    let loaded = load(&path).unwrap();
    assert_eq!(loaded.samples, vec![boundary, new]);
    assert_eq!(loaded.skipped, 0);
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
}

#[test]
fn prune_missing_file_is_noop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");
    assert_eq!(prune(&path, Utc::now()).unwrap(), 0);
    assert!(!path.exists());
}

#[test]
fn prune_concurrent_with_append_no_lost_writes() {
    let cutoff = Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
    let future = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();

    for _iter in 0..50 {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        append(&path, &[make_sample("init", 1.0)]).unwrap(); // 2026 ts: always pruned

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(11));
        let mut handles = Vec::new();
        {
            let (p, b) = (path.clone(), barrier.clone());
            handles.push(std::thread::spawn(move || {
                b.wait();
                prune(&p, cutoff).unwrap();
            }));
        }
        for i in 0..10 {
            let (p, b) = (path.clone(), barrier.clone());
            handles.push(std::thread::spawn(move || {
                b.wait();
                append(&p, &[sample_at(&format!("app-{i}"), future)]).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 0);
        let mut ids: Vec<_> = loaded.samples.iter().map(|s| s.bucket_id.clone()).collect();
        ids.sort();
        let mut expected: Vec<_> = (0..10).map(|i| format!("app-{i}")).collect();
        expected.sort();
        assert_eq!(ids, expected, "lost or extra samples");
    }
}
```

(Add `use chrono::DateTime;`, `use std::io::Write;`, and `use std::os::unix::fs::PermissionsExt;` to the test module imports as needed.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test store::tests`
Expected: FAIL to compile (`oldest_timestamp` / `prune` not found).

- [ ] **Step 3: Implement sidecar lock, `oldest_timestamp`, and `prune` in `src/store.rs`**

- Helpers:
  - `fn lock_path_for(path: &Path) -> PathBuf { path.with_extension("lock") }`
  - `fn lock_sidecar(path: &Path, op: FlockOperation) -> io::Result<File>`: opens `lock_path_for(path)` with `create(true).read(true).write(true)`, calls `flock(&file, op)`, returns the file as the guard (lock released on drop/close).
  - Extract the existing line-parsing loop from `load` into a shared helper (`parse_line(&[u8]) -> Option<Result<Sample, ()>>` or similar) so `load`, `oldest_timestamp`, `prune`, and `load_from` (Task 3) agree on trimming/CRLF/UTF-8 handling.
- `append`: `create_dir_all(parent)` → **acquire `LockExclusive` on the sidecar** → open `samples.jsonl` with `O_APPEND` → `write_all` → drop data file → drop lock guard. Remove the flock on the data file itself.
- `load`: if `!path.exists()` return empty *before* touching the sidecar (the data dir may not exist) → acquire `LockShared` on the sidecar → open and read as today.
- `oldest_timestamp(path)`: missing file → `Ok(None)`. Otherwise `LockShared` on sidecar, read lines with `read_until(b'\n', ...)`, return the `ts` of the **first line that parses as a `Sample`**, skipping empty/corrupt/non-UTF-8 lines. Return `Ok(None)` if no valid line exists.
- `prune(path, cutoff)`:
  - Missing file → `Ok(0)` without creating anything.
  - Acquire `LockExclusive` on the sidecar and hold it until after rename.
  - Read `path` line-by-line; keep samples with `ts >= cutoff`; count removed valid samples; drop corrupt lines.
  - Write kept records (re-serialized, one per line) into `tempfile::NamedTempFile::new_in(parent)?`.
  - Copy permissions from the original file's metadata onto the temp file (`NamedTempFile` defaults to `0600`).
  - `temp.as_file().sync_all()?`, then `temp.persist(path)?` (map `PersistError` to its inner `io::Error`).
  - Best-effort `File::open(parent)?.sync_all()` for rename durability.
  - Drop the lock guard; return the removed count.

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test store::`
Expected: PASS (all store tests, including the existing `concurrent_append`).

- [ ] **Step 5: Commit Task 2**

```bash
git add src/store.rs
git commit -m "feat(store): sidecar lock protocol, oldest_timestamp peeking, and atomic prune"
```

---

### Task 3: Incremental Store Reader for TUI

**Files:**
- Modify: `src/store.rs`
- Modify: `src/tui.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
  pub struct Cursor { pub ino: u64, pub offset: u64 }

  #[derive(Debug, PartialEq, Clone)]
  pub struct Incremental { pub loaded: Loaded, pub cursor: Cursor, pub reset: bool }

  pub fn load_from(path: &Path, cursor: Cursor) -> std::io::Result<Incremental>
  ```
- In `tui.rs`: integrates `load_from` with `cursor: Cursor` and `accumulated_samples: Vec<Sample>`.

- [ ] **Step 1: Write failing tests in `src/store.rs`**

```rust
#[test]
fn load_from_incremental() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");

    let s1 = make_sample("b1", 0.9);
    append(&path, &[s1.clone()]).unwrap();
    let r1 = load_from(&path, Cursor::default()).unwrap();
    assert_eq!(r1.loaded.samples, vec![s1]);
    assert!(r1.reset); // first load from the default cursor is a reset
    assert!(r1.cursor.offset > 0);

    let s2 = make_sample("b2", 0.8);
    append(&path, &[s2.clone()]).unwrap();
    let r2 = load_from(&path, r1.cursor).unwrap();
    assert_eq!(r2.loaded.samples, vec![s2]);
    assert!(!r2.reset);
    assert!(r2.cursor.offset > r1.cursor.offset);

    let r3 = load_from(&path, r2.cursor).unwrap();
    assert!(r3.loaded.samples.is_empty());
    assert!(!r3.reset);
    assert_eq!(r3.cursor, r2.cursor);
}

#[test]
fn load_from_resets_after_prune_rename_even_if_file_grew() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");
    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let new_ts = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();

    let old = sample_at("old", Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap());
    let kept = sample_at("kept", new_ts);
    append(&path, &[old, kept.clone()]).unwrap();
    let before = load_from(&path, Cursor::default()).unwrap();

    prune(&path, cutoff).unwrap();
    let extra: Vec<_> = (0..3).map(|i| sample_at(&format!("x{i}"), new_ts)).collect();
    append(&path, &extra).unwrap();
    assert!(fs::metadata(&path).unwrap().len() > before.cursor.offset);

    let after = load_from(&path, before.cursor).unwrap();
    assert!(after.reset);
    let mut expected = vec![kept];
    expected.extend(extra);
    assert_eq!(after.loaded.samples, expected);
}

#[test]
fn load_from_resets_after_in_place_truncation() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");
    append(&path, &[make_sample("b1", 0.9), make_sample("b2", 0.8)]).unwrap();
    let before = load_from(&path, Cursor::default()).unwrap();

    let s3 = make_sample("b3", 0.7);
    fs::write(&path, serde_json::to_string(&s3).unwrap() + "\n").unwrap(); // same inode, shorter

    let after = load_from(&path, before.cursor).unwrap();
    assert!(after.reset);
    assert_eq!(after.loaded.samples, vec![s3]);
    assert!(after.cursor.offset < before.cursor.offset);
}

#[test]
fn load_from_does_not_consume_partial_trailing_line() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");
    let s1 = make_sample("b1", 0.9);
    let line = serde_json::to_string(&s1).unwrap() + "\n";
    fs::write(&path, format!("{line}{{\"ts\":\"2026-")).unwrap();

    let r = load_from(&path, Cursor::default()).unwrap();
    assert_eq!(r.loaded.samples, vec![s1]);
    assert_eq!(r.loaded.skipped, 0);
    assert_eq!(r.cursor.offset, line.len() as u64);
}

#[test]
fn load_from_missing_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");
    let r = load_from(&path, Cursor { ino: 42, offset: 100 }).unwrap();
    assert!(r.loaded.samples.is_empty());
    assert!(r.reset);
    assert_eq!(r.cursor, Cursor::default());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test store::tests::load_from`
Expected: FAIL to compile (`load_from` / `Cursor` not found).

- [ ] **Step 3: Implement `load_from` in `src/store.rs`**

- Missing file → `Incremental { loaded: empty, cursor: Cursor::default(), reset: cursor != Cursor::default() }`.
- Acquire `LockShared` on the sidecar, then open `samples.jsonl`.
- `let meta = file.metadata()?; let ino = meta.ino();` (`std::os::unix::fs::MetadataExt`).
- If `ino != cursor.ino || meta.len() < cursor.offset` → `reset = true`, `start = 0`; else `reset = false`, `start = cursor.offset`.
- Seek to `start`; read with `read_until(b'\n', ...)`. Only lines ending in `\n` are parsed and counted toward `consumed`; a trailing line without `\n` is left unconsumed (neither parsed nor skipped) so the next call re-reads it.
- Return `Incremental { loaded, cursor: Cursor { ino, offset: start + consumed }, reset }`.
- Doc comment: note the residual edge case that inode reuse across two prunes between reloads could defeat `ino` detection; acceptable because prunes are at most daily and the TUI reloads every 30 s.

- [ ] **Step 4: Integrate `load_from` into `src/tui.rs`**

- In `run()`:
  - Initialize `let mut cursor = crate::store::Cursor::default();` and `let mut accumulated_samples: Vec<Sample> = Vec::new();`.
  - Replace the two `crate::store::load(&samples_path)` call sites (the `RefreshOutcome::Success` branch and the 30-second reload) with a shared helper:
    ```rust
    fn reload(
        path: &Path,
        cursor: &mut crate::store::Cursor,
        acc: &mut Vec<Sample>,
        tight: f64,
    ) -> Option<Vec<BucketReport>> {
        let inc = crate::store::load_from(path, *cursor).ok()?;
        if inc.reset {
            *acc = inc.loaded.samples;
        } else if inc.loaded.samples.is_empty() {
            *cursor = inc.cursor;
            return None; // nothing new; keep existing reports
        } else {
            acc.extend(inc.loaded.samples);
        }
        *cursor = inc.cursor;
        Some(crate::forecast::analyze(acc, tight))
    }
    ```
  - Assign `reports` only when the helper returns `Some`. The initial load happens through the existing `last_reload = now - 60s` path.

- [ ] **Step 5: Run all unit tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit Task 3**

```bash
git add src/store.rs src/tui.rs
git commit -m "feat(tui,store): inode-aware incremental store reading with reset on prune"
```

---

### Task 4: CLI `--retention-days`, Validation, Timer Propagation & Lazy Pruning

**Files:**
- Modify: `src/main.rs`
- Modify: `src/timer.rs`
- Modify: `tests/cli.rs`

**Interfaces:**
- Produces: `--retention-days` in `Cli`, `effective_retention_days()` validation in `main()`, lazy pruning in `collect`, and propagation in `timer::unit_files` / `timer::install`.

- [ ] **Step 1: Write failing tests**

In `src/timer.rs`, add:

```rust
#[test]
fn unit_files_propagates_retention_days() {
    let (service, _timer) = unit_files(
        Path::new("/x/agy-usage"),
        Path::new("/usr/bin/agy"),
        Path::new("/d"),
        0.10,
        30,
        120,
    )
    .unwrap();
    assert!(service.contains("--history-days 30 --retention-days 120 collect"));
}
```

Update the existing timer tests for the new signature (`unit_files(exe, agy, data, tight, history, retention)`):
- `unit_files_contents`: pass `90`; expected `ExecStart=` ends with `--history-days 30 --retention-days 90 collect`.
- `unit_files_escapes_systemd_directives`: pass `90`; expected `ExecStart=` ends with `--history-days 14 --retention-days 90 collect`.
- `unit_files_rejects_control_characters`: add `90` to all four calls.
- `unit_files_resolves_relative_paths`: pass `90`; expected substring `--tight-threshold 0.1 --history-days 30 --retention-days 90 collect`.

In `tests/cli.rs` (std `Command`, no new dependencies):

```rust
fn run_html(data: &std::path::Path, extra: &[&str]) -> std::process::Output {
    let out_file = data.join("out.html");
    let mut args: Vec<String> = vec!["--data-dir".into(), data.display().to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    args.extend(["html".into(), "--out".into(), out_file.display().to_string()]);
    Command::new(env!("CARGO_BIN_EXE_agy-usage")).args(&args).output().unwrap()
}

#[test]
fn cli_rejects_retention_days_below_minimum() {
    let dir = tempdir().unwrap();
    let out = run_html(dir.path(), &["--retention-days", "7"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("8..=36500"), "unexpected stderr: {stderr}");
}

#[test]
fn cli_rejects_explicit_retention_less_than_history() {
    let dir = tempdir().unwrap();
    let out = run_html(dir.path(), &["--history-days", "60", "--retention-days", "30"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("cannot be less than --history-days"), "unexpected stderr: {stderr}");
}

#[test]
fn cli_accepts_explicit_retention_equal_to_history() {
    let dir = tempdir().unwrap();
    let out = run_html(dir.path(), &["--history-days", "30", "--retention-days", "30"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// Regression guard: units installed before Phase 3 pass --history-days without
/// --retention-days. These must keep working even when history > 90.
#[test]
fn cli_accepts_large_history_without_retention_flag() {
    let dir = tempdir().unwrap();
    let out = run_html(dir.path(), &["--history-days", "120"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
```

Also extend `cli_collect_scenarios` (or add a new test using the existing fake-`agy` script pattern) to verify lazy pruning end-to-end: seed `samples.jsonl` with one sample dated 200 days ago, run `collect --no-html` with the fixture-backed fake `agy`, and assert the old sample is gone while the freshly collected samples are present.

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test`
Expected: FAIL. Timer tests fail to compile (arity); `cli_rejects_retention_days_below_minimum`, `cli_rejects_explicit_retention_less_than_history`, `cli_accepts_explicit_retention_equal_to_history`, and the lazy-prune test fail (unknown flag / no pruning). `cli_accepts_large_history_without_retention_flag` is expected to PASS before and after (regression guard).

- [ ] **Step 3: Implement `--retention-days`, validation, lazy pruning, and timer propagation**

In `src/main.rs`:
- Add to `Cli`:
  ```rust
  /// Days of raw samples to keep on disk [default: max(90, --history-days)]
  #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(8..=36500))]
  retention_days: Option<u32>,
  ```
- Add a pure helper (unit-tested in `main.rs`'s `#[cfg(test)] mod tests`):
  ```rust
  fn effective_retention_days(explicit: Option<u32>, history_days: u32) -> Result<u32> {
      match explicit {
          Some(r) if r < history_days => bail!(
              "--retention-days ({r}) cannot be less than --history-days ({history_days})"
          ),
          Some(r) => Ok(r),
          None => Ok(history_days.max(90)),
      }
  }
  ```
  Unit tests: `(None, 30) → 90`, `(None, 120) → 120`, `(Some(30), 30) → 30`, `(Some(30), 60) → Err`.
- In `main()`, compute `let retention_days = effective_retention_days(cli.retention_days, cli.history_days)?;` before dispatching any command and before any side effects.
- In `Commands::Collect`, directly after `store::append(...)` and **before** the `if !no_html` block (so pruning also runs with `--no-html`), reusing the existing `now`:
  ```rust
  let retention = chrono::Duration::days(i64::from(retention_days));
  let prune_threshold = now - retention - chrono::Duration::days(1);
  match agy_usage::store::oldest_timestamp(&samples_path) {
      Ok(Some(oldest)) if oldest <= prune_threshold => {
          match agy_usage::store::prune(&samples_path, now - retention) {
              Ok(n) => eprintln!("pruned {n} samples older than {retention_days} days"),
              Err(e) => eprintln!("warning: failed to prune {}: {e}", samples_path.display()),
          }
      }
      Ok(_) => {}
      Err(e) => eprintln!("warning: failed to inspect {}: {e}", samples_path.display()),
  }
  ```
- In `Commands::InstallTimer`, pass `retention_days` (the effective value) to `timer::install`.

In `src/timer.rs`:
- Add `retention_days: u32` as the last parameter of `unit_files` and `install`.
- `ExecStart=` format becomes `... --history-days {} --retention-days {} collect`. The value is a validated `u32`, so no escaping is required.

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test`
Expected: PASS.

- [ ] **Step 5: Update README**

Document `--retention-days` (default `max(90, --history-days)`, min `8`), lazy daily pruning behaviour, and that re-running `install-timer` propagates an explicit value into the unit.

- [ ] **Step 6: Commit Task 4**

```bash
git add src/main.rs src/timer.rs tests/cli.rs README.md
git commit -m "feat(cli,timer): add upgrade-safe --retention-days with validation and lazy pruning"
```

---

### Task 5: Full Verification, Pre-Push Review Gates, and PR Update

**Files:** All modified files.

- [ ] **Step 1: Run comprehensive local verification suite**

Run:
```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
Expected: All tests pass, 0 clippy warnings, 0 fmt diffs. Run `cargo test store::tests::prune_concurrent_with_append_no_lost_writes` 5 more times to check for flakiness.

- [ ] **Step 2: Conduct Pass 1 Senior Code Review subagent**

Invoke `Senior Reviewer` subagent for Pass 1 with the Review Focus list above.
Address all findings and add regression tests.

- [ ] **Step 3: Conduct Pass 2 Senior Code Review subagent**

Invoke `Senior Reviewer` subagent for Pass 2 to verify fixes and check for unintended regressions.

- [ ] **Step 4: Push to `feat/agy-usage` and update PR #1**

```bash
git push origin feat/agy-usage
```

- [ ] **Step 5: Request and watch Copilot review on PR #1**

Request `@copilot review` on PR #1.
Watch GitHub Actions CI and Copilot workflow runs reactively via `gh run watch <RUN_ID> --exit-status`.
Address every comment, self-review the fixes, resolve threads via the `resolveReviewThread` GraphQL mutation, and re-request review until clean.
