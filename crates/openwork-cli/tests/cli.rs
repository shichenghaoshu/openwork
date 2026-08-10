use std::fs;
use std::process::Command;

fn openwork() -> Command {
    Command::new(env!("CARGO_BIN_EXE_openwork"))
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
