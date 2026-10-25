use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn cli_collect_scenarios() {
    let bin_path = env!("CARGO_BIN_EXE_agy-usage");
    let script_dir = tempdir().unwrap();

    // 1. Successful agy returning fixture
    let fake_agy_ok = script_dir.path().join("fake_agy_ok.sh");
    let fixture_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/usage.json");
    let script_content = format!("#!/bin/sh\ncat \"{}\"\n", fixture_path);
    fs::write(&fake_agy_ok, script_content).unwrap();
    fs::set_permissions(&fake_agy_ok, fs::Permissions::from_mode(0o755)).unwrap();

    let data_dir1 = tempdir().unwrap();
    let data_path1 = data_dir1.path();

    // Run 1: collect 4 buckets
    let status1 = Command::new(bin_path)
        .arg("--data-dir")
        .arg(data_path1)
        .arg("collect")
        .env("AGY_BIN", &fake_agy_ok)
        .status()
        .expect("failed to run agy-usage collect");
    assert!(status1.success());

    let samples_file = data_path1.join("samples.jsonl");
    assert!(samples_file.exists());
    let lines1: Vec<_> = fs::read_to_string(&samples_file)
        .unwrap()
        .lines()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(lines1.len(), 4);
    assert!(data_path1.join("dashboard.html").exists());

    // Run 2: second collect appends 4 more buckets
    let status2 = Command::new(bin_path)
        .arg("--data-dir")
        .arg(data_path1)
        .arg("collect")
        .env("AGY_BIN", &fake_agy_ok)
        .status()
        .expect("failed to run agy-usage collect");
    assert!(status2.success());
    let lines2: Vec<_> = fs::read_to_string(&samples_file)
        .unwrap()
        .lines()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(lines2.len(), 8);

    // 2. agy command exits with error code 1
    let fake_agy_fail = script_dir.path().join("fake_agy_fail.sh");
    fs::write(&fake_agy_fail, "#!/bin/sh\necho 'error' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&fake_agy_fail, fs::Permissions::from_mode(0o755)).unwrap();

    let data_dir2 = tempdir().unwrap();
    let data_path2 = data_dir2.path();
    let status_fail = Command::new(bin_path)
        .arg("--data-dir")
        .arg(data_path2)
        .arg("collect")
        .env("AGY_BIN", &fake_agy_fail)
        .status()
        .expect("failed to run agy-usage collect");
    assert!(!status_fail.success());
    assert!(!data_path2.join("samples.jsonl").exists());

    // 3. agy outputs fixture with all buckets disabled / remaining_fraction removed
    let raw_fixture = include_str!("../tests/fixtures/usage.json");
    let mut fixture_val: serde_json::Value = serde_json::from_str(raw_fixture).unwrap();
    if let Some(groups) = fixture_val
        .pointer_mut("/command/data/groups")
        .and_then(|v| v.as_array_mut())
    {
        for g in groups {
            if let Some(buckets) = g.get_mut("buckets").and_then(|v| v.as_array_mut()) {
                for b in buckets {
                    if let Some(obj) = b.as_object_mut() {
                        obj.remove("remaining_fraction");
                    }
                }
            }
        }
    }
    let disabled_fixture = serde_json::to_string(&fixture_val).unwrap();
    let fake_agy_disabled = script_dir.path().join("fake_agy_disabled.sh");
    fs::write(
        &fake_agy_disabled,
        format!("#!/bin/sh\ncat << 'EOF'\n{}\nEOF\n", disabled_fixture),
    )
    .unwrap();
    fs::set_permissions(&fake_agy_disabled, fs::Permissions::from_mode(0o755)).unwrap();

    let data_dir3 = tempdir().unwrap();
    let data_path3 = data_dir3.path();
    let status_disabled = Command::new(bin_path)
        .arg("--data-dir")
        .arg(data_path3)
        .arg("collect")
        .env("AGY_BIN", &fake_agy_disabled)
        .status()
        .expect("failed to run agy-usage collect");
    assert!(!status_disabled.success());
    assert!(!data_path3.join("samples.jsonl").exists());
}

#[test]
fn cli_validations() {
    let bin_path = env!("CARGO_BIN_EXE_agy-usage");
    let dir = tempdir().unwrap();

    // 1. Negative history_days is rejected by clap (u32 parse failure)
    let status_neg = Command::new(bin_path)
        .arg("--history-days")
        .arg("-5")
        .arg("show")
        .arg("--once")
        .status()
        .expect("failed to run agy-usage");
    assert!(!status_neg.success());

    // 2. Out-of-range history_days (4294967295) is rejected by clap range parser
    let status_huge = Command::new(bin_path)
        .arg("--history-days")
        .arg("4294967295")
        .arg("show")
        .arg("--once")
        .status()
        .expect("failed to run agy-usage");
    assert!(!status_huge.success());

    // 3. Directory passed to AGY_BIN is rejected by resolve_agy_bin in install-timer
    let status_dir = Command::new(bin_path)
        .env("AGY_BIN", dir.path())
        .env("PATH", "")
        .arg("install-timer")
        .status()
        .expect("failed to run agy-usage");
    assert!(!status_dir.success());

    // 4. Non-executable regular file passed to AGY_BIN is rejected by resolve_agy_bin
    let non_exec_file = dir.path().join("fake_agy_nonexec");
    fs::write(&non_exec_file, "not executable").unwrap();
    fs::set_permissions(&non_exec_file, fs::Permissions::from_mode(0o644)).unwrap();
    let status_nonexec = Command::new(bin_path)
        .env("AGY_BIN", &non_exec_file)
        .env("PATH", "")
        .arg("install-timer")
        .status()
        .expect("failed to run agy-usage");
    assert!(!status_nonexec.success());

    // 5. Invalid AGY_BIN fails fast even if a valid executable 'agy' exists on PATH
    let valid_path_dir = dir.path().join("valid_bin");
    fs::create_dir_all(&valid_path_dir).unwrap();
    let valid_agy = valid_path_dir.join("agy");
    fs::write(&valid_agy, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&valid_agy, fs::Permissions::from_mode(0o755)).unwrap();

    let output_failfast = Command::new(bin_path)
        .env("AGY_BIN", &non_exec_file)
        .env("PATH", &valid_path_dir)
        .arg("install-timer")
        .output()
        .expect("failed to run agy-usage");
    assert!(!output_failfast.status.success());
    let stderr_failfast = String::from_utf8_lossy(&output_failfast.stderr);
    assert!(
        stderr_failfast.contains("AGY_BIN is set to"),
        "expected fail-fast error, got: {}",
        stderr_failfast
    );

    // 6. Empty PATH element does not pick up ./agy in working directory
    let run_dir = tempdir().unwrap();
    let local_agy = run_dir.path().join("agy");
    fs::write(&local_agy, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&local_agy, fs::Permissions::from_mode(0o755)).unwrap();

    let output_emptypath = Command::new(bin_path)
        .current_dir(run_dir.path())
        .env_remove("AGY_BIN")
        .env("PATH", ":::/nonexistent::")
        .arg("install-timer")
        .output()
        .expect("failed to run agy-usage");
    assert!(!output_emptypath.status.success());
    let stderr_emptypath = String::from_utf8_lossy(&output_emptypath.stderr);
    assert!(
        stderr_emptypath.contains("could not find executable 'agy' on PATH"),
        "expected PATH not found error, got: {}",
        stderr_emptypath
    );
}

fn run_html(data: &std::path::Path, extra: &[&str]) -> std::process::Output {
    let out_file = data.join("out.html");
    let mut args: Vec<String> = vec!["--data-dir".into(), data.display().to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    args.extend([
        "html".into(),
        "--out".into(),
        out_file.display().to_string(),
    ]);
    Command::new(env!("CARGO_BIN_EXE_agy-usage"))
        .args(&args)
        .output()
        .unwrap()
}

fn html_clock(out: &std::process::Output, data: &std::path::Path) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = std::fs::read_to_string(data.join("out.html")).unwrap();
    let start = html.find("\"clock\":\"").expect("clock missing") + "\"clock\":\"".len();
    html[start..start + 3].to_string()
}

#[test]
fn cli_clock_flag_overrides_detection() {
    let dir = tempdir().unwrap();
    let out = run_html(dir.path(), &["--clock", "12h"]);
    assert_eq!(html_clock(&out, dir.path()), "12h");
    let out = run_html(dir.path(), &["--clock", "24h"]);
    assert_eq!(html_clock(&out, dir.path()), "24h");
}

#[test]
fn cli_clock_flag_rejects_unknown_value() {
    let dir = tempdir().unwrap();
    let out = run_html(dir.path(), &["--clock", "13h"]);
    assert!(!out.status.success());
}

fn run_html_with_path(data: &std::path::Path, path_env: &std::path::Path) -> String {
    let out_file = data.join("out.html");
    let out = Command::new(env!("CARGO_BIN_EXE_agy-usage"))
        .args(["--data-dir", &data.display().to_string()])
        .args(["html", "--out", &out_file.display().to_string()])
        .env("PATH", path_env)
        .output()
        .unwrap();
    html_clock(&out, data)
}

/// Fake probe that only answers when invoked with exactly `expected_args`.
fn fake_probe(dir: &std::path::Path, name: &str, expected_args: &str, output: &str) {
    let p = dir.join(name);
    let script = format!(
        "#!/bin/sh\n[ \"$*\" = \"{expected_args}\" ] || exit 1\nprintf '%s\\n' \"{output}\"\n"
    );
    fs::write(&p, script).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn cli_clock_auto_falls_back_to_24h_without_probes() {
    let dir = tempdir().unwrap();
    // Neither gsettings nor locale can be found.
    let empty_bin = tempdir().unwrap();
    assert_eq!(run_html_with_path(dir.path(), empty_bin.path()), "24h");
}

#[test]
fn cli_clock_auto_prefers_desktop_setting_over_locale() {
    let dir = tempdir().unwrap();
    let bin = tempdir().unwrap();
    fake_probe(
        bin.path(),
        "gsettings",
        "get org.gnome.desktop.interface clock-format",
        "'12h'",
    );
    fake_probe(bin.path(), "locale", "t_fmt", "%T");
    assert_eq!(run_html_with_path(dir.path(), bin.path()), "12h");
}

#[test]
fn cli_clock_auto_uses_locale_without_desktop_setting() {
    let dir = tempdir().unwrap();
    let bin = tempdir().unwrap();
    fake_probe(bin.path(), "locale", "t_fmt", "%r");
    assert_eq!(run_html_with_path(dir.path(), bin.path()), "12h");
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
    let out = run_html(
        dir.path(),
        &["--history-days", "60", "--retention-days", "30"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("cannot be less than --history-days"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn cli_accepts_explicit_retention_equal_to_history() {
    let dir = tempdir().unwrap();
    let out = run_html(
        dir.path(),
        &["--history-days", "30", "--retention-days", "30"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Regression guard: units installed before Phase 3 pass --history-days without
/// --retention-days. These must keep working even when history > 90.
#[test]
fn cli_accepts_large_history_without_retention_flag() {
    let dir = tempdir().unwrap();
    let out = run_html(dir.path(), &["--history-days", "120"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cli_lazy_pruning_on_collect() {
    let bin_path = env!("CARGO_BIN_EXE_agy-usage");
    let script_dir = tempdir().unwrap();
    let fake_agy_ok = script_dir.path().join("fake_agy_ok.sh");
    let fixture_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/usage.json");
    fs::write(
        &fake_agy_ok,
        format!("#!/bin/sh\ncat \"{}\"\n", fixture_path),
    )
    .unwrap();
    fs::set_permissions(&fake_agy_ok, fs::Permissions::from_mode(0o755)).unwrap();

    let data_dir = tempdir().unwrap();
    let samples_file = data_dir.path().join("samples.jsonl");

    // Seed with a sample 200 days old
    let old_ts = chrono::Utc::now() - chrono::Duration::days(200);
    let old_sample = agy_usage::model::Sample {
        ts: old_ts,
        group: "Old Group".to_string(),
        bucket_id: "old_bucket".to_string(),
        window: "5h".to_string(),
        remaining: 0.5,
        reset_time: Some(old_ts + chrono::Duration::hours(5)),
        disabled: false,
    };
    agy_usage::store::append(&samples_file, std::slice::from_ref(&old_sample)).unwrap();

    // Run collect with --retention-days 90
    let status = Command::new(bin_path)
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("--retention-days")
        .arg("90")
        .arg("collect")
        .arg("--no-html")
        .env("AGY_BIN", &fake_agy_ok)
        .status()
        .expect("failed to run agy-usage collect");
    assert!(status.success());

    let loaded = agy_usage::store::load(&samples_file).unwrap();
    assert_eq!(loaded.samples.len(), 4);
    assert!(!loaded.samples.iter().any(|s| s.bucket_id == "old_bucket"));
}

fn is_yyyy_mm_dd(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn verify_release_sync(
    ver: &str,
    readme: &str,
    changelog: &str,
    cargo_lock: &str,
) -> Result<(), String> {
    let expected_url = format!(
        "https://github.com/danieljov/agy-usage/releases/download/v{ver}/agy-usage-v{ver}-x86_64-unknown-linux-gnu.tar.gz"
    );
    if !readme.contains(&expected_url) {
        return Err(format!(
            "README.md is missing expected release tarball URL for v{ver}: {expected_url}"
        ));
    }

    // Ensure no stale release download URLs or tarball filenames remain in README.md
    for (idx, _) in readme.match_indices("releases/download/v") {
        let after = &readme[idx + "releases/download/v".len()..];
        let found_ver = after.split('/').next().unwrap_or("");
        if found_ver != ver {
            return Err(format!(
                "README.md contains stale release version v{found_ver} (expected v{ver})"
            ));
        }
    }
    for (idx, _) in readme.match_indices("agy-usage-v") {
        let after = &readme[idx + "agy-usage-v".len()..];
        let found_ver = after.split("-x86_64").next().unwrap_or("");
        if found_ver != ver {
            return Err(format!(
                "README.md contains stale tarball version v{found_ver} (expected v{ver})"
            ));
        }
    }

    let expected_header = format!("## [{ver}] - ");
    let latest_release_line = changelog
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("## [") && *line != "## [Unreleased]");
    match latest_release_line {
        Some(line) if line.starts_with(&expected_header) => {
            let date_part = &line[expected_header.len()..];
            if !is_yyyy_mm_dd(date_part) {
                return Err(format!(
                    "CHANGELOG.md release header '{line}' does not have a valid YYYY-MM-DD date"
                ));
            }
        }
        _ => {
            return Err(format!(
                "CHANGELOG.md is missing release section header '{expected_header}<YYYY-MM-DD>' as latest release"
            ));
        }
    }

    let expected_link_prefix = format!("[{ver}]: https://github.com/danieljov/agy-usage/");
    let has_matching_link = changelog.lines().map(str::trim).any(|line| {
        line.starts_with(&expected_link_prefix)
            && (line.ends_with(&format!("...v{ver}")) || line.ends_with(&format!("/v{ver}")))
    });
    if !has_matching_link {
        return Err(format!(
            "CHANGELOG.md is missing comparison/release link for [{ver}] ending with v{ver}"
        ));
    }

    let expected_version_line = format!("version = \"{ver}\"");
    let has_lock_entry = cargo_lock
        .lines()
        .zip(cargo_lock.lines().skip(1))
        .any(|(l1, l2)| l1.trim() == "name = \"agy-usage\"" && l2.trim() == expected_version_line);
    if !has_lock_entry {
        return Err(format!(
            "Cargo.lock version for agy-usage does not match Cargo.toml version {ver}"
        ));
    }

    Ok(())
}

#[test]
fn release_metadata_and_docs_stay_in_sync() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let ver = env!("CARGO_PKG_VERSION");
    let readme = fs::read_to_string(format!("{manifest_dir}/README.md")).unwrap();
    let changelog = fs::read_to_string(format!("{manifest_dir}/CHANGELOG.md")).unwrap();
    let cargo_lock = fs::read_to_string(format!("{manifest_dir}/Cargo.lock")).unwrap();

    if let Err(err) = verify_release_sync(ver, &readme, &changelog, &cargo_lock) {
        panic!("Release metadata out of sync with Cargo.toml (v{ver}): {err}");
    }

    // Negative boundary assertions: any forgotten file on a version bump must fail
    let next_ver = "9.9.9";
    assert!(
        verify_release_sync(next_ver, &readme, &changelog, &cargo_lock)
            .unwrap_err()
            .contains("README.md is missing expected release tarball URL")
    );

    let updated_readme = readme.replace(&format!("v{ver}"), &format!("v{next_ver}"));
    assert!(
        verify_release_sync(next_ver, &updated_readme, &changelog, &cargo_lock)
            .unwrap_err()
            .contains("CHANGELOG.md is missing release section header")
    );

    let stale_readme = format!(
        "{updated_readme}\nhttps://github.com/danieljov/agy-usage/releases/download/v0.1.0/agy-usage-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
    );
    assert!(
        verify_release_sync(next_ver, &stale_readme, &changelog, &cargo_lock)
            .unwrap_err()
            .contains("README.md contains stale release version v0.1.0")
    );

    let updated_changelog_no_link = changelog.replace(
        "## [Unreleased]",
        &format!("## [Unreleased]\n\n## [{next_ver}] - 2026-10-22"),
    );
    assert!(
        verify_release_sync(
            next_ver,
            &updated_readme,
            &updated_changelog_no_link,
            &cargo_lock
        )
        .unwrap_err()
        .contains("CHANGELOG.md is missing comparison/release link")
    );

    let updated_changelog_full = format!(
        "{updated_changelog_no_link}\n[{next_ver}]: https://github.com/danieljov/agy-usage/compare/v{ver}...v{next_ver}\n"
    );
    assert!(
        verify_release_sync(
            next_ver,
            &updated_readme,
            &updated_changelog_full,
            &cargo_lock
        )
        .unwrap_err()
        .contains("Cargo.lock version for agy-usage does not match")
    );

    let updated_lock = cargo_lock.replace(
        &format!("name = \"agy-usage\"\nversion = \"{ver}\""),
        &format!("name = \"agy-usage\"\nversion = \"{next_ver}\""),
    );
    assert!(
        verify_release_sync(
            next_ver,
            &updated_readme,
            &updated_changelog_full,
            &updated_lock
        )
        .is_ok()
    );

    // CRLF line endings in Cargo.lock are handled cleanly
    let crlf_lock = updated_lock.replace('\n', "\r\n");
    assert!(
        verify_release_sync(
            next_ver,
            &updated_readme,
            &updated_changelog_full,
            &crlf_lock
        )
        .is_ok()
    );

    // Asymmetric desync: CHANGELOG.md bumped to 9.9.9 while Cargo.toml is still at current `ver`
    assert!(
        verify_release_sync(ver, &readme, &updated_changelog_full, &cargo_lock)
            .unwrap_err()
            .contains("CHANGELOG.md is missing release section header")
    );
}

#[test]
fn cli_version_flags() {
    let bin_path = env!("CARGO_BIN_EXE_agy-usage");
    let expected = format!("agy-usage {}\n", env!("CARGO_PKG_VERSION"));

    for flag in ["--version", "-V"] {
        let out = Command::new(bin_path)
            .arg(flag)
            .env_remove("HOME")
            .env_remove("XDG_DATA_HOME")
            .output()
            .expect("failed to execute agy-usage with version flag");
        assert!(
            out.status.success(),
            "expected {flag} to succeed, stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
        assert!(
            out.stderr.is_empty(),
            "expected empty stderr for {flag}, got: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn release_workflow_configures_idempotent_crates_io_publish() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workflow =
        fs::read_to_string(format!("{manifest_dir}/.github/workflows/release.yml")).unwrap();
    let readme = fs::read_to_string(format!("{manifest_dir}/README.md")).unwrap();

    // OIDC Trusted Publishing permission alongside contents: write
    assert!(workflow.contains("contents: write"));
    assert!(workflow.contains("id-token: write"));

    // Verify step ordering: tag check -> lints -> tests -> GitHub release -> tarball cleanup -> cargo publish
    let idx_tag_check = workflow
        .find("Verify tag matches Cargo.toml")
        .expect("missing tag check step");
    let idx_lints = workflow
        .find("Check formatting and lints")
        .expect("missing lints step");
    let idx_tests = workflow
        .find("Run test suite")
        .expect("missing test suite step");
    let idx_gh_release = workflow
        .find("Upload to GitHub Release")
        .expect("missing GitHub Release step");
    let idx_cleanup = workflow
        .find("rm -rf agy-usage-")
        .expect("missing tarball cleanup before cargo publish");
    let idx_publish = workflow
        .find("cargo publish --locked")
        .expect("missing cargo publish --locked command");

    assert!(idx_tag_check < idx_lints);
    assert!(idx_lints < idx_tests);
    assert!(idx_tests < idx_gh_release);
    assert!(idx_gh_release < idx_cleanup);
    assert!(idx_cleanup < idx_publish);

    // Sparse index check and already-uploaded fallback for idempotency
    assert!(workflow.contains("https://index.crates.io/ag/y-/agy-usage"));
    assert!(workflow.contains("already uploaded|already exists"));
    assert!(workflow.contains("CARGO_REGISTRY_TOKEN"));
    assert!(workflow.contains("rust-lang/crates-io-auth-action@v1"));
    assert!(workflow.contains("steps.crates-check.outputs.has_token != 'true'"));
    assert!(workflow.contains("PKG_VER: ${{ steps.crates-check.outputs.pkg_ver }}"));
    assert!(!workflow.contains("PKG_VER=\"${{"));

    // README documents --version, TUI range keybinding, and crates.io release setup
    assert!(readme.contains("-V, --version"));
    assert!(readme.contains("`t`") || readme.contains("`w`"));
    assert!(readme.contains("CARGO_REGISTRY_TOKEN"));
    assert!(readme.contains("Trusted Publishing"));
}

#[test]
fn cli_html_and_show_once_load_only_the_history_window() {
    let data_dir = tempdir().unwrap();
    let samples_file = data_dir.path().join("samples.jsonl");
    let now = chrono::Utc::now();
    let mk = |ts: chrono::DateTime<chrono::Utc>, bucket_id: &str, remaining: f64| {
        agy_usage::model::Sample {
            ts,
            group: "Gemini Models".to_string(),
            bucket_id: bucket_id.to_string(),
            window: "5h".to_string(),
            remaining,
            reset_time: Some(ts + chrono::Duration::hours(3)),
            disabled: false,
        }
    };
    // Stale bucket last seen 60 days ago (beyond --history-days 30 + margin), and a
    // corrupt line among the old records that must not be reported.
    let stale = agy_usage::model::Sample {
        group: "Retired Models".to_string(),
        ..mk(now - chrono::Duration::days(60), "stale-bucket-5h", 0.4)
    };
    let recent = [
        mk(now - chrono::Duration::minutes(10), "live-bucket-5h", 0.9),
        mk(now - chrono::Duration::minutes(5), "live-bucket-5h", 0.8),
    ];
    agy_usage::store::append(&samples_file, std::slice::from_ref(&stale)).unwrap();
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&samples_file)
            .unwrap();
        f.write_all(b"{\"ts\":\"2000-01-01T00:00:00Z\",\"group\":\"truncated\n")
            .unwrap();
    }
    agy_usage::store::append(&samples_file, &recent).unwrap();

    let out = run_html(data_dir.path(), &["--clock", "24h", "--history-days", "30"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = fs::read_to_string(data_dir.path().join("out.html")).unwrap();
    assert!(html.contains("\"bucket_id\":\"live-bucket-5h\""));
    assert!(!html.contains("stale-bucket-5h"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("skipped 1 corrupt lines"));

    // A long enough --history-days brings the stale bucket back.
    let out = run_html(data_dir.path(), &["--clock", "24h", "--history-days", "90"]);
    assert!(out.status.success());
    let html = fs::read_to_string(data_dir.path().join("out.html")).unwrap();
    assert!(html.contains("\"bucket_id\":\"stale-bucket-5h\""));
    assert!(html.contains("\"bucket_id\":\"live-bucket-5h\""));

    let out = Command::new(env!("CARGO_BIN_EXE_agy-usage"))
        .arg("--data-dir")
        .arg(data_dir.path())
        .args(["--clock", "24h", "show", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The table shows groups, not bucket ids.
    assert!(stdout.contains("Gemini Models"), "{stdout}");
    assert!(!stdout.contains("Retired Models"), "{stdout}");

    // The raw file is untouched by bounded loading.
    let loaded = agy_usage::store::load(&samples_file).unwrap();
    assert_eq!(loaded.samples.len(), 3);
    assert_eq!(loaded.skipped, 1);
}

#[test]
fn cli_html_uses_full_history_for_unknown_window_buckets() {
    let data_dir = tempdir().unwrap();
    let samples_file = data_dir.path().join("samples.jsonl");
    let now = chrono::Utc::now();
    let reset = now + chrono::Duration::days(10);
    // An unknown "monthly" window draining for 20 days with no reset in between. Its
    // prognosis regresses over the whole window, which a 7 + 8 day load would cut.
    let samples: Vec<agy_usage::model::Sample> = (0..=20 * 24)
        .map(|h| {
            let ts = now - chrono::Duration::hours(20 * 24 - h);
            // Accelerating burn so a truncated regression gives a different slope.
            let x = h as f64 / (20.0 * 24.0);
            agy_usage::model::Sample {
                ts,
                group: "Gemini Models".to_string(),
                bucket_id: "gemini-monthly".to_string(),
                window: "monthly".to_string(),
                remaining: 1.0 - 0.6 * x * x,
                reset_time: Some(reset),
                disabled: false,
            }
        })
        .collect();
    agy_usage::store::append(&samples_file, &samples).unwrap();
    let expected = agy_usage::forecast::analyze(&samples, 0.10);
    let expected_rate = expected[0].prognosis.rate_per_h.unwrap();

    let out = run_html(data_dir.path(), &["--clock", "24h", "--history-days", "7"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = fs::read_to_string(data_dir.path().join("out.html")).unwrap();
    let start = html.find("const data = ").unwrap() + "const data = ".len();
    let data: serde_json::Value = serde_json::Deserializer::from_str(&html[start..])
        .into_iter::<serde_json::Value>()
        .next()
        .unwrap()
        .unwrap();
    let rep = &data["reports"][0];
    assert_eq!(rep["bucket_id"], "gemini-monthly");
    let rate = rep["prognosis"]["rate_per_h"].as_f64().unwrap();
    assert!(
        (rate - expected_rate).abs() < 1e-12,
        "rate {rate} != full-history rate {expected_rate}"
    );
    // The displayed series is still limited to --history-days.
    let series = rep["series"].as_array().unwrap();
    assert!(series.len() <= 7 * 24 + 1, "{}", series.len());
}
