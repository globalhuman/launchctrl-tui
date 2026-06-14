use std::{fs, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    emit_current_branch_rerun_hint();

    emit_git_env(
        "LAUNCHCTRL_TUI_GIT_HASH",
        &["rev-parse", "--short", "HEAD"],
        "unknown",
    );
    emit_git_env(
        "LAUNCHCTRL_TUI_GIT_BRANCH",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "unknown",
    );
    emit_git_env(
        "LAUNCHCTRL_TUI_GIT_TAG",
        &["describe", "--tags", "--exact-match", "HEAD"],
        "untagged",
    );
}

fn emit_current_branch_rerun_hint() {
    let Ok(head) = fs::read_to_string(".git/HEAD") else {
        return;
    };

    let Some(ref_name) = head.trim().strip_prefix("ref: ") else {
        return;
    };

    println!("cargo:rerun-if-changed=.git/{ref_name}");
}

fn emit_git_env(name: &str, args: &[&str], fallback: &str) {
    let value = git_output(args).unwrap_or_else(|| fallback.to_string());
    println!("cargo:rustc-env={name}={value}");
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() || value == "HEAD" {
        None
    } else {
        Some(value)
    }
}
