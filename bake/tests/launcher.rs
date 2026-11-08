// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#[test]
fn discovers_project_tasks() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_base-benchmark-load-bake"))
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success());
    let tasks = String::from_utf8(output.stdout).unwrap();
    assert!(tasks.contains("test:coverage"));
    assert!(tasks.contains("cargo:after_version_bump"));
}
