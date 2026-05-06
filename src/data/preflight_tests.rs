use super::*;
use crate::state::test_fs_lock;
use std::ffi::OsStr;

fn with_temp_dir<T>(f: impl FnOnce() -> T) -> T {
    let _guard = test_fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::TempDir::new().unwrap();
    let prev = std::env::current_dir().unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::env::set_current_dir(dir.path()).unwrap();
        f()
    }));

    std::env::set_current_dir(prev).unwrap();
    result.unwrap()
}

#[test]
fn test_detect_project_type_rust() {
    with_temp_dir(|| {
        fs::write("Cargo.toml", "[package]\nname = \"test\"").unwrap();
        let content = generate_heuristic_gitignore(".codexize/");
        assert!(content.contains("# Rust"));
        assert!(content.contains("target/"));
    });
}

#[test]
fn test_has_existing_files_empty() {
    with_temp_dir(|| {
        assert!(!has_existing_files());
    });
}

#[test]
fn test_has_existing_files_with_dotfile() {
    with_temp_dir(|| {
        fs::write(".hidden", "").unwrap();
        assert!(!has_existing_files());
    });
}

#[test]
fn test_has_existing_files_with_regular_file() {
    with_temp_dir(|| {
        fs::write("file.txt", "content").unwrap();
        assert!(has_existing_files());
    });
}

#[test]
fn test_generate_heuristic_gitignore_contains_codexize() {
    let content = generate_heuristic_gitignore(".codexize/");
    assert!(content.contains(".codexize/"));
    assert!(content.contains(".DS_Store"));
}

#[test]
fn test_append_to_gitignore_creates_file() {
    with_temp_dir(|| {
        append_to_gitignore(".codexize/").unwrap();
        let content = fs::read_to_string(".gitignore").unwrap();
        assert!(content.contains(".codexize/"));
    });
}

#[test]
fn test_append_to_gitignore_appends() {
    with_temp_dir(|| {
        fs::write(".gitignore", "node_modules/").unwrap();
        append_to_gitignore(".codexize/").unwrap();
        let content = fs::read_to_string(".gitignore").unwrap();
        assert!(content.contains("node_modules/"));
        assert!(content.contains(".codexize/"));
    });
}

#[test]
fn claude_acp_install_root_uses_home_codexize_acp() {
    let _guard = test_fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    let home = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::set_var("HOME", home.path());
    }

    let root = crate::acp::claude_acp_install_root();

    unsafe {
        match prev_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    assert_eq!(root, home.path().join(".codexize").join("acp"));
}

#[test]
fn detect_ignored_accepts_required_directory_entry_before_dir_exists() {
    with_temp_dir(|| {
        git_cmd(&["init"]);
        fs::write(".gitignore", ".codexize/\n").unwrap();

        assert!(detect_ignored(Path::new(".codexize")));
    });
}

#[test]
fn detect_ignored_accepts_required_entry_when_old_session_file_is_tracked() {
    with_temp_dir(|| {
        git_cmd(&["init"]);
        fs::write(".gitignore", ".codexize/\n").unwrap();
        fs::create_dir_all(".codexize/sessions/old/rounds/001").unwrap();
        fs::write(
            ".codexize/sessions/old/rounds/001/coder_summary.toml",
            "status = \"done\"\n",
        )
        .unwrap();
        git_cmd(&["add", ".gitignore"]);
        git_cmd(&[
            "add",
            "-f",
            ".codexize/sessions/old/rounds/001/coder_summary.toml",
        ]);

        assert!(detect_ignored(Path::new(".codexize")));
    });
}

#[test]
fn gitignore_generation_is_deterministic_without_runtime_launch() {
    with_temp_dir(|| {
        fs::write("Cargo.toml", "[package]\nname = \"demo\"\n").unwrap();

        let fake_bin = Path::new("fake-bin");
        fs::create_dir_all(fake_bin).unwrap();
        let codex_log = Path::new("codex.log");
        write_fake_executable(
            &fake_bin.join("codex"),
            &format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
                codex_log.display()
            ),
        );

        let original_path = std::env::var_os("PATH");
        // SAFETY: serialized via test_fs_lock and restored below.
        unsafe {
            std::env::set_var("PATH", fake_bin);
        }

        let outcome = std::panic::catch_unwind(|| generate_gitignore_preflight_file(".codexize/"));

        unsafe {
            match original_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }

        let finish_marker = outcome
            .expect("gitignore generation should not panic")
            .expect("gitignore generation should succeed");
        let content = fs::read_to_string(".gitignore").expect("read generated gitignore");
        assert!(content.contains(".codexize/"));
        assert!(content.contains("target/"));
        assert!(
            finish_marker.exists(),
            "expected finish marker to be written"
        );
        assert!(
            !codex_log.exists(),
            "preflight gitignore generation must not launch agent CLIs"
        );
    });
}

fn git_cmd(args: &[&str]) {
    let status = Command::new("git").args(args).status().unwrap();
    assert!(
        status.success(),
        "git command failed: git {}",
        args.join(" ")
    );
}

fn git_output(args: &[&str]) -> String {
    let output = Command::new("git").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git command failed: git {}",
        args.join(" ")
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn git_output_allow_failure(args: &[&str], env: &[(&str, &OsStr)]) -> (bool, String, String) {
    let mut cmd = Command::new("git");
    cmd.args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn init_repo_with_head() {
    git_cmd(&["init"]);
    git_cmd(&["config", "user.name", "Test User"]);
    git_cmd(&["config", "user.email", "test@example.com"]);
    fs::write("README.md", "seed\n").unwrap();
    git_cmd(&["add", "README.md"]);
    git_cmd(&["commit", "-m", "seed"]);
}

fn write_fake_executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}

#[test]
fn gitignore_modal_clean_repo_auto_commits_with_fixed_subject() {
    with_temp_dir(|| {
        init_repo_with_head();
        append_to_gitignore(".codexize/").unwrap();
        maybe_auto_commit_gitignore(|_| {});

        assert_eq!(
            git_output(&["log", "-1", "--format=%s"]),
            GITIGNORE_AUTO_COMMIT_SUBJECT
        );
        assert_eq!(git_output(&["status", "--porcelain"]), "");
        let tracked = git_output(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(tracked, ".gitignore");
    });
}

#[test]
fn gitignore_modal_staged_gitignore_still_auto_commits() {
    with_temp_dir(|| {
        init_repo_with_head();
        fs::write(".gitignore", "target/\nlogs/\n").unwrap();
        git_cmd(&["add", ".gitignore"]);
        git_cmd(&["commit", "-m", "add gitignore"]);
        fs::write(".gitignore", "target/\nlogs/\ncache/\n").unwrap();
        git_cmd(&["add", ".gitignore"]);
        append_to_gitignore(".codexize/").unwrap();
        maybe_auto_commit_gitignore(|_| {});

        assert_eq!(
            git_output(&["log", "-1", "--format=%s"]),
            GITIGNORE_AUTO_COMMIT_SUBJECT
        );
        assert_eq!(git_output(&["status", "--porcelain"]), "");
        let content = fs::read_to_string(".gitignore").unwrap();
        assert!(content.contains("target/"));
        assert!(content.contains(".codexize/"));
    });
}

#[test]
fn gitignore_modal_dirty_repo_skips_auto_commit() {
    with_temp_dir(|| {
        init_repo_with_head();
        let previous_head = git_output(&["rev-parse", "HEAD"]);
        fs::write("README.md", "dirty\n").unwrap();
        append_to_gitignore(".codexize/").unwrap();
        maybe_auto_commit_gitignore(|_| {});

        assert_eq!(git_output(&["rev-parse", "HEAD"]), previous_head);
        let status = git_output(&["status", "--porcelain"]);
        assert!(status.contains(".gitignore"));
    });
}

#[test]
fn gitignore_modal_only_codexize_changes_skips_auto_commit() {
    with_temp_dir(|| {
        init_repo_with_head();
        let previous_head = git_output(&["rev-parse", "HEAD"]);
        fs::create_dir(".codexize").unwrap();
        fs::write(".codexize/note.txt", "internal").unwrap();
        maybe_auto_commit_gitignore(|_| {});

        assert_eq!(git_output(&["rev-parse", "HEAD"]), previous_head);
    });
}

#[test]
fn gitignore_modal_missing_identity_is_swallowed_and_warned() {
    with_temp_dir(|| {
        git_cmd(&["init"]);
        git_cmd(&["config", "user.name", ""]);
        git_cmd(&["config", "user.email", ""]);
        let fake_home = tempfile::TempDir::new().unwrap();
        let empty_global = fake_home.path().join("empty-gitconfig");
        fs::write(&empty_global, "").unwrap();

        let env = [
            ("HOME", fake_home.path().as_os_str()),
            ("XDG_CONFIG_HOME", fake_home.path().as_os_str()),
            ("GIT_CONFIG_GLOBAL", empty_global.as_os_str()),
            ("GIT_CONFIG_NOSYSTEM", OsStr::new("1")),
        ];

        append_to_gitignore(".codexize/").unwrap();
        let mut warnings = Vec::new();
        maybe_auto_commit_gitignore(|w| warnings.push(w));

        let (head_ok, _stdout, _stderr) = git_output_allow_failure(&["rev-parse", "HEAD"], &env);
        assert!(
            !head_ok,
            "no commit should be created when identity is missing"
        );
        assert!(
            warnings.iter().any(|w| {
                w.contains("identity")
                    || w.contains("user.email")
                    || w.contains("user.name")
                    || w.contains("unable to auto-detect email address")
            }),
            "expected identity warning, got: {warnings:?}"
        );
    });
}
