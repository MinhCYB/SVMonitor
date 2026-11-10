//! Focused integration tests for the native UI CLI bridge (`svmonitor ui`).
//!
//! Validates daemon refusal when not running, name validation rules,
//! clap argument constraints, and launcher dispatch in isolated environments.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;

struct TestHarness {
    dir: TempDir,
    config_path: PathBuf,
    runtime_dir: PathBuf,
    bin_dir: PathBuf,
}

impl TestHarness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create tempdir");
        let config_dir = dir.path().join("config").join("svmonitor");
        let state_dir = dir.path().join("state").join("svmonitor");
        let run_base = dir.path().join("run");
        let runtime_dir = run_base.join("svmonitor");
        let bin_dir = dir.path().join("bin");

        fs::create_dir_all(&config_dir).expect("create config_dir");
        fs::create_dir_all(&state_dir).expect("create state_dir");
        fs::create_dir_all(&bin_dir).expect("create bin_dir");
        fs::create_dir_all(&run_base).expect("create run_base");
        fs::set_permissions(&run_base, fs::Permissions::from_mode(0o700))
            .expect("set run_base permissions");

        let config_path = config_dir.join("config.toml");

        Self {
            dir,
            config_path,
            runtime_dir,
            bin_dir,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let bin = env!("CARGO_BIN_EXE_svmonitor");
        let mut cmd = Command::new(bin);
        cmd.arg("--config").arg(&self.config_path);
        for a in args {
            cmd.arg(a);
        }

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let path = format!("{}:{}", self.bin_dir.display(), existing_path);

        cmd.env("PATH", path)
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_RUNTIME_DIR", self.runtime_dir.parent().unwrap());

        cmd.output().expect("failed to execute svmonitor")
    }
}

#[test]
fn no_daemon_refusal() {
    let harness = TestHarness::new();

    let commands: &[&[&str]] = &[
        &["ui", "check"],
        &["ui", "check", "myserver"],
        &["ui", "pause", "myserver"],
        &["ui", "resume", "myserver"],
        &["ui", "remove", "myserver"],
        &["ui", "add", "myserver"],
    ];

    for cmd in commands {
        let out = harness.run(cmd);
        assert!(
            !out.status.success(),
            "command {:?} should fail when no daemon is running",
            cmd
        );
        assert_eq!(
            out.status.code(),
            Some(3),
            "command {:?} should exit with code 3 on daemon error",
            cmd
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("SVMonitor daemon is not running")
                || stderr.contains("No such file or directory"),
            "command {:?} stderr should mention daemon not running, got: {}",
            cmd,
            stderr
        );
    }
}

#[test]
fn invalid_name_rejection() {
    let harness = TestHarness::new();

    let invalid_names = [
        "-dash",
        "--double-dash",
        "has space",
        "has\ttab",
        "has\nnewline",
        "semi;colon",
        "pipe|symbol",
        "amp&ersand",
        "back`tick",
        "dollar$sign",
        "single'quote",
        "double\"quote",
        "star*glob",
        "quest?ion",
        "bang!",
        "less<than",
        "greater>than",
        "",
    ];

    for name in invalid_names {
        // Test check
        let out = harness.run(&["ui", "check", "--", name]);
        assert!(
            !out.status.success(),
            "ui check should reject invalid name {name:?}"
        );
        assert_eq!(out.status.code(), Some(3));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("server name"),
            "stderr for {name:?} should mention server name validation error: {stderr}"
        );

        // Test pause
        let out = harness.run(&["ui", "pause", "--", name]);
        assert!(
            !out.status.success(),
            "ui pause should reject invalid name {name:?}"
        );
        assert_eq!(out.status.code(), Some(3));

        // Test resume
        let out = harness.run(&["ui", "resume", "--", name]);
        assert!(
            !out.status.success(),
            "ui resume should reject invalid name {name:?}"
        );
        assert_eq!(out.status.code(), Some(3));

        // Test remove
        let out = harness.run(&["ui", "remove", "--", name]);
        assert!(
            !out.status.success(),
            "ui remove should reject invalid name {name:?}"
        );
        assert_eq!(out.status.code(), Some(3));

        // Test add
        let out = harness.run(&["ui", "add", "--", name]);
        assert!(
            !out.status.success(),
            "ui add should reject invalid name {name:?}"
        );
        assert_eq!(out.status.code(), Some(3));
    }

    // Name exceeding 253 characters
    let long_name = "a".repeat(254);
    let out = harness.run(&["ui", "pause", &long_name]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("server name must be 1-253 characters"));
}

#[test]
fn missing_required_arguments_rejected() {
    let harness = TestHarness::new();

    let missing_arg_cmds: &[&[&str]] = &[
        &["ui", "pause"],
        &["ui", "resume"],
        &["ui", "remove"],
        &["ui", "add"],
        &["ui", "settings", "unexpected"],
    ];

    for cmd in missing_arg_cmds {
        let out = harness.run(cmd);
        assert!(
            !out.status.success(),
            "command {:?} should fail clap validation",
            cmd
        );
        // Clap exits with code 2 on missing or unexpected arguments
        assert_eq!(
            out.status.code(),
            Some(2),
            "command {:?} should exit with clap code 2",
            cmd
        );
    }
}
