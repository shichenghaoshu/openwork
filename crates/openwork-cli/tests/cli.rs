use std::fs;
use std::path::Path;
use std::process::Command;

fn openwork() -> Command {
    Command::new(env!("CARGO_BIN_EXE_openwork"))
}

fn isolate_home(command: &mut Command, root: &Path) {
    command
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("APPDATA", root.join("AppData/Roaming"))
        .env("LOCALAPPDATA", root.join("AppData/Local"))
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("XDG_DATA_HOME", root.join(".local/share"))
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .env("XDG_STATE_HOME", root.join(".local/state"));
}

#[test]
fn version_is_stable() {
    let output = openwork().arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "OpenWork 0.1.0\n"
    );
}

#[test]
fn doctor_json_is_structured() {
    let output = openwork().args(["doctor", "--json"]).output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert!(
        value["checks"]
            .as_array()
            .is_some_and(|checks| !checks.is_empty())
    );
}

#[test]
fn install_dry_run_has_no_filesystem_side_effects() {
    let home = tempfile::tempdir().unwrap();
    let output = openwork()
        .args(["install", "--dry-run", "--json"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn runtime_install_preview_uses_the_managed_plan_without_side_effects() {
    let home = tempfile::tempdir().unwrap();
    let output = openwork()
        .args(["install", "--dry-run", "--runtime", "claude", "--json"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert!(value["steps"].as_array().is_some_and(|steps| {
        steps
            .iter()
            .any(|step| step["id"] == "runtime.claude-code.download.0")
    }));
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn execute_requires_explicit_consent() {
    let output = openwork().args(["install", "--execute"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn successful_bootstrap_persists_lockfile_and_installed_status() {
    let home = tempfile::tempdir().unwrap();
    let mut install = openwork();
    install.args(["install", "--execute", "--yes", "--json"]);
    isolate_home(&mut install, home.path());
    let output = install.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let execution: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(execution["completed"], true);

    let mut status = openwork();
    status.args(["status", "--json"]);
    isolate_home(&mut status, home.path());
    let output = status.output().unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["state"], "installed");
    assert_eq!(value["lockfile"]["schemaVersion"], 1);
    assert_eq!(value["lockfile"]["runtimes"].as_object().unwrap().len(), 0);
}

#[test]
fn runtime_commands_expose_registered_and_error_states() {
    let list = openwork()
        .args(["runtime", "list", "--json"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let runtimes: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(runtimes[0]["metadata"]["id"], "claude-code");

    let claude = openwork()
        .args(["runtime", "info", "claude-code", "--json"])
        .output()
        .unwrap();
    assert!(claude.status.success());
    let summary: serde_json::Value = serde_json::from_slice(&claude.stdout).unwrap();
    assert_eq!(summary["metadata"]["distribution"], "external_managed");

    let codex = openwork()
        .args(["runtime", "info", "codex", "--json"])
        .output()
        .unwrap();
    assert!(codex.status.success());
    let summary: serde_json::Value = serde_json::from_slice(&codex.stdout).unwrap();
    assert_eq!(
        summary["metadata"]["upstream"],
        "https://github.com/openai/codex"
    );

    let info = openwork()
        .args(["runtime", "info", "missing", "--json"])
        .output()
        .unwrap();
    assert_eq!(info.status.code(), Some(20));
    let error: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(error["code"], "runtime_not_found");
}
