# Phase 3: Long-Term Scalability & Offline Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement Phase 3 long-term scalability and offline resilience by adding a sliding retention window with sidecar-locked atomic pruning, incremental store loading for the TUI, lazy prune triggers on collection, and vendored offline Chart.js bundles.

**Architecture:** A sidecar lock file (`samples.lock`) coordinates exclusive access during writes and atomic file rewrites (`prune`), while shared locks allow concurrent readers. The TUI tracks byte offsets to load only newly appended records $O(\Delta)$ with automatic truncation recovery. `assets/vendor/` embeds minified Chart.js and date adapter directly into `dashboard.html` for zero-network offline rendering.

**Tech Stack:** Rust 2024, `rustix::fs::flock`, `tempfile::NamedTempFile`, `serde_json`, `chrono`, `clap`, `ratatui`.

**Spec:** [`docs/superpowers/specs/2026-10-11-phase-3-scalability-and-offline-design.md`](file:///home/daniel/projects/ai_resources/docs/superpowers/specs/2026-10-11-phase-3-scalability-and-offline-design.md)

## Global Constraints

- Never break or change the sample JSONL format (`ts`, `group`, `bucket_id`, `window`, `remaining`, `reset_time`).
- All storage file mutations must hold `FlockOperation::LockExclusive` on `samples.lock` (derived via `path.with_extension("lock")`).
- `store::prune` must write kept samples to a temporary file in `path.parent()` and atomically rename over `samples.jsonl` while holding the sidecar lock.
- `--retention-days` defaults to `90`, with minimum value `8`.
- If `--retention-days < --history-days`, CLI must error and exit non-zero.
- The generated HTML must be 100% self-contained with zero external `<script src="...">` tags.
- Full verification (`cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`) must pass with zero errors and zero warnings.

## Review Focus

1. **Concurrent Append & Prune Race**: If `prune` renames `samples.jsonl` while an append is executing, the append must not write to the unlinked inode. Synchronizing on `samples.lock` ensures appends block until rename completes and open the new inode.
2. **Compaction Truncation in TUI**: If `load_from` is called with `offset > file_len` (due to daily pruning), it must not fail or return empty; it must reset offset to 0 and reload all surviving records.
3. **Empty / Corrupt Lines During Prune**: `prune` must tolerate and skip corrupt lines, preserving all valid samples on or after the cutoff timestamp.
4. **Offline HTML Fallback**: The vendored Chart.js must initialize cleanly in browsers without network access, including boundary markers and tooltips.
5. **Timer Directive Escaping**: The propagated `--retention-days` in `ExecStart=` must preserve systemd directive encoding without double expansion.

---

### Task 1: Vendor Offline Chart.js & Date Adapter

**Files:**
- Create: `assets/vendor/chart.umd.min.js`
- Create: `assets/vendor/chartjs-adapter-date-fns.bundle.min.js`
- Modify: `assets/dashboard.html`
- Modify: `src/html.rs`

**Interfaces:**
- Consumes: `assets/vendor/*.min.js`
- Produces: `html::render(reports, generated_at, history_days) -> String` with embedded inline JavaScript.

- [ ] **Step 1: Download production vendor bundles into `assets/vendor/`**

```bash
mkdir -p assets/vendor
curl -sL https://cdn.jsdelivr.net/npm/chart.js@4/dist/chart.umd.min.js -o assets/vendor/chart.umd.min.js
curl -sL https://cdn.jsdelivr.net/npm/chartjs-adapter-date-fns@3/dist/chartjs-adapter-date-fns.bundle.min.js -o assets/vendor/chartjs-adapter-date-fns.bundle.min.js
```

- [ ] **Step 2: Write failing test in `src/html.rs` verifying zero external CDN script tags and presence of inlined scripts**

```rust
#[test]
fn renders_with_inlined_scripts_zero_external_cdns() {
    let reports = make_test_reports();
    let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
    let html = render(&reports, now, 30);

    assert!(!html.contains("cdn.jsdelivr.net"));
    assert!(!html.contains("<script src="));
    assert!(html.contains("Chart.register"));
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test html::tests::renders_with_inlined_scripts_zero_external_cdns`
Expected: FAIL because `dashboard.html` currently contains `cdn.jsdelivr.net`.

- [ ] **Step 4: Update `assets/dashboard.html` and `src/html.rs` to inline vendored bundles**

In `assets/dashboard.html`:
Replace `<script src="https://cdn.jsdelivr.net/npm/..."></script>` with `<script>/*__CHART_JS__*/</script>` and `<script>/*__DATE_FNS_ADAPTER__*/</script>`.

In `src/html.rs`:
```rust
const CHART_JS: &str = include_str!("../assets/vendor/chart.umd.min.js");
const DATE_FNS_ADAPTER: &str = include_str!("../assets/vendor/chartjs-adapter-date-fns.bundle.min.js");
```
And replace the placeholders in `render()`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test html::`
Expected: PASS (all 9 html tests pass).

- [ ] **Step 6: Commit Task 1**

```bash
git add assets/vendor assets/dashboard.html src/html.rs
git commit -m "feat(html): vendor Chart.js and date adapter for 100% offline dashboards"
```

---

### Task 2: Sidecar Lock Protocol, Store Pruning, and Oldest Timestamp

**Files:**
- Modify: `src/store.rs`

**Interfaces:**
- Produces:
  - `store::append(path: &Path, samples: &[Sample]) -> std::io::Result<()>` (uses `samples.lock`)
  - `store::load(path: &Path) -> std::io::Result<Loaded>` (uses `samples.lock`)
  - `store::oldest_timestamp(path: &Path) -> std::io::Result<Option<DateTime<Utc>>>`
  - `store::prune(path: &Path, cutoff: DateTime<Utc>) -> std::io::Result<usize>` (returns pruned count)

- [ ] **Step 1: Write failing tests in `src/store.rs`**

```rust
#[test]
fn oldest_timestamp_reads_first_line() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");

    assert_eq!(oldest_timestamp(&path).unwrap(), None);

    let s1 = make_sample("b1", 0.9);
    let s2 = make_sample("b2", 0.8);
    append(&path, &[s1.clone(), s2.clone()]).unwrap();

    assert_eq!(oldest_timestamp(&path).unwrap(), Some(s1.ts));
}

#[test]
fn prune_filters_old_records_atomically() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");

    let mut old_sample = make_sample("b1", 0.9);
    old_sample.ts = Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap();

    let mut new_sample = make_sample("b2", 0.8);
    new_sample.ts = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();

    append(&path, &[old_sample, new_sample.clone()]).unwrap();

    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let pruned = prune(&path, cutoff).unwrap();
    assert_eq!(pruned, 1);

    let loaded = load(&path).unwrap();
    assert_eq!(loaded.samples, vec![new_sample]);
}

#[test]
fn prune_concurrent_with_append_no_lost_writes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");

    let initial = make_sample("init", 1.0);
    append(&path, &[initial.clone()]).unwrap();

    let path_clone = path.clone();
    let prune_handle = std::thread::spawn(move || {
        let cutoff = Utc::now() + chrono::Duration::hours(1);
        let _ = prune(&path_clone, cutoff);
    });

    let mut appends = Vec::new();
    for i in 0..10 {
        let p = path.clone();
        appends.push(std::thread::spawn(move || {
            let s = make_sample(&format!("app-{}", i), 0.5);
            append(&p, &[s]).unwrap();
        }));
    }

    prune_handle.join().unwrap();
    for h in appends {
        h.join().unwrap();
    }

    let loaded = load(&path).unwrap();
    assert_eq!(loaded.skipped, 0);
    assert!(!loaded.samples.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test store::tests::oldest_timestamp_reads_first_line`
Expected: FAIL with "cannot find function `oldest_timestamp`".

- [ ] **Step 3: Implement sidecar lock, `oldest_timestamp`, and `prune` in `src/store.rs`**

- Helper `fn lock_path_for(path: &Path) -> std::path::PathBuf { path.with_extension("lock") }`
- Update `append` and `load` to flock `lock_path_for(path)` with `LockExclusive` / `LockShared`.
- Implement `oldest_timestamp(path)`: opens with `LockShared`, peeks first line with `read_until(b'\n', ...)`, parses `Sample`, returns `Ok(Some(s.ts))`.
- Implement `prune(path, cutoff)`:
  - Takes `LockExclusive` on `lock_path_for(path)`.
  - Reads `path` line-by-line, filters `ts >= cutoff`.
  - Writes kept records to `tempfile::NamedTempFile::new_in(parent)?`.
  - Calls `file.as_file().sync_all()?`.
  - Renames temp file over `path` via `temp_file.persist(path)?`.
  - Unlocks sidecar lock and returns number of pruned samples.

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test store::`
Expected: PASS (all store tests pass).

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
- Produces: `store::load_from(path: &Path, offset: u64) -> std::io::Result<(Loaded, u64)>`
- In `tui.rs`: integrates `load_from` with `last_offset: u64` and `accumulated_samples: Vec<Sample>`.

- [ ] **Step 1: Write failing test in `src/store.rs`**

```rust
#[test]
fn load_from_incremental_and_truncation_recovery() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("samples.jsonl");

    let s1 = make_sample("b1", 0.9);
    append(&path, &[s1.clone()]).unwrap();

    let (loaded1, offset1) = load_from(&path, 0).unwrap();
    assert_eq!(loaded1.samples, vec![s1.clone()]);
    assert!(offset1 > 0);

    let s2 = make_sample("b2", 0.8);
    append(&path, &[s2.clone()]).unwrap();

    let (loaded2, offset2) = load_from(&path, offset1).unwrap();
    assert_eq!(loaded2.samples, vec![s2.clone()]);
    assert!(offset2 > offset1);

    // Truncate file (simulate prune rewrite)
    let s3 = make_sample("b3", 0.7);
    fs::write(&path, serde_json::to_string(&s3).unwrap() + "\n").unwrap();

    // Passing offset2 > new file len triggers automatic recovery from offset 0
    let (loaded3, offset3) = load_from(&path, offset2).unwrap();
    assert_eq!(loaded3.samples, vec![s3]);
    assert!(offset3 > 0 && offset3 < offset2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test store::tests::load_from_incremental_and_truncation_recovery`
Expected: FAIL with "cannot find function `load_from`".

- [ ] **Step 3: Implement `load_from(path: &Path, offset: u64) -> std::io::Result<(Loaded, u64)>` in `src/store.rs`**

- Checks `file.metadata()?.len()`. If `file_len < offset`, sets `actual_offset = 0`.
- Seeks to `actual_offset`.
- Reads lines, parses `Sample`s.
- Returns `(Loaded { samples, skipped }, file.stream_position()?)`.

- [ ] **Step 4: Integrate `load_from` into `src/tui.rs`**

- In `run()`:
  - Initialize `let mut last_offset: u64 = 0;` and `let mut accumulated_samples: Vec<Sample> = Vec::new();`.
  - Define reload closure/helper:
    ```rust
    if let Ok((loaded, new_offset)) = crate::store::load_from(&samples_path, last_offset) {
        if new_offset < last_offset {
            accumulated_samples = loaded.samples;
        } else {
            accumulated_samples.extend(loaded.samples);
        }
        last_offset = new_offset;
        reports = crate::forecast::analyze(&accumulated_samples, tight);
    }
    ```
  - Call on startup, in the 30-second reload check, and when `RefreshOutcome::Success` is received.

- [ ] **Step 5: Run all unit tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit Task 3**

```bash
git add src/store.rs src/tui.rs
git commit -m "feat(tui,store): incremental store reading with compaction truncation recovery"
```

---

### Task 4: CLI `--retention-days`, Validation, Timer Propagation & Lazy Pruning

**Files:**
- Modify: `src/main.rs`
- Modify: `src/timer.rs`
- Modify: `tests/cli.rs`

**Interfaces:**
- Produces: `--retention-days` in `Cli`, validation in `main()`, lazy pruning in `collect`, and propagation in `timer::unit_files`.

- [ ] **Step 1: Write failing CLI validation tests in `tests/cli.rs` and timer test in `src/timer.rs`**

In `src/timer.rs`:
```rust
#[test]
fn unit_files_propagates_retention_days() {
    let service = unit_files("/usr/bin/agy-usage", "/tmp/data", 0.10, 30, 90, "/usr/bin/agy").0;
    assert!(service.contains("--retention-days 90"));
}
```

In `tests/cli.rs`:
```rust
#[test]
fn cli_rejects_retention_days_less_than_eight() {
    let mut cmd = Command::cargo_bin("agy-usage").unwrap();
    cmd.args(["--retention-days", "7", "collect"]);
    cmd.assert().failure();
}

#[test]
fn cli_rejects_retention_days_less_than_history_days() {
    let mut cmd = Command::cargo_bin("agy-usage").unwrap();
    cmd.args(["--history-days", "60", "--retention-days", "30", "collect"]);
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("cannot be less than --history-days"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test cli_rejects_retention_days`
Expected: FAIL.

- [ ] **Step 3: Implement `--retention-days`, validation, lazy pruning, and timer propagation**

In `src/main.rs`:
- Add `retention_days` to `Cli` struct with default `90`, value parser `8..=36500`.
- In `main()`:
  ```rust
  if cli.retention_days < cli.history_days {
      bail!(
          "--retention-days ({}) cannot be less than --history-days ({})",
          cli.retention_days,
          cli.history_days
      );
  }
  ```
- In `Commands::Collect`:
  After appending samples, check:
  ```rust
  let prune_threshold = Utc::now() - Duration::days(i64::from(cli.retention_days) + 1);
  if let Ok(Some(oldest)) = store::oldest_timestamp(&samples_path)
      && oldest <= prune_threshold
  {
      let cutoff = Utc::now() - Duration::days(i64::from(cli.retention_days));
      let _ = store::prune(&samples_path, cutoff);
  }
  ```
- In `Commands::InstallTimer`:
  Pass `cli.retention_days` to `timer::install`.

In `src/timer.rs`:
- Update `install` and `unit_files` to accept `retention_days: u32`.
- Include `--retention-days {retention_days}` in `ExecStart=`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit Task 4**

```bash
git add src/main.rs src/timer.rs tests/cli.rs
git commit -m "feat(cli,timer): add --retention-days flag with validation and lazy pruning in collect"
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
Expected: All tests pass, 0 clippy warnings, 0 fmt diffs.

- [ ] **Step 2: Conduct Pass 1 Senior Code Review subagent**

Invoke `Senior Reviewer` subagent for Pass 1.
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
Verify clean approval from GitHub Copilot.
