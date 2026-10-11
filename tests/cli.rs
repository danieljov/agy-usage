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
