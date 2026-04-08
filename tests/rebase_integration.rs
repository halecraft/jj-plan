//! Integration test for `Workspace::rebase_bookmark_onto_trunk`.
//!
//! Creates a real jj workspace with a git remote, builds a multi-bookmark
//! stack, then simulates a GitHub-style squash-merge by creating a NEW
//! commit on trunk (not the original bookmark commit). This forces
//! `rebase_bookmark_onto_trunk` to actually move commits, exercising the
//! code path that panicked with "BUG: Descendants have not been rebased
//! after the last rewrites" when `rebase_descendants()` was missing after
//! `move_commits()`.
//!
//! Run:
//! ```sh
//! cargo test --test rebase_integration
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

/// Find the real jj binary (not the shim).
fn real_jj() -> &'static str {
    if Path::new("/opt/homebrew/bin/jj").exists() {
        "/opt/homebrew/bin/jj"
    } else {
        "jj"
    }
}

/// Run a jj command in the given repo dir. Panics on non-zero exit.
fn jj(repo: &Path, args: &[&str]) -> String {
    let output = Command::new(real_jj())
        .args(args)
        .current_dir(repo)
        .env("JJ_USER", "Test User")
        .env("JJ_EMAIL", "test@example.com")
        .output()
        .unwrap_or_else(|e| panic!("failed to run jj {}: {e}", args.join(" ")));

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        panic!(
            "jj {} failed (exit {}):\nstdout: {stdout}\nstderr: {stderr}",
            args.join(" "),
            output.status
        );
    }
    stdout
}

/// Run a git command in the given dir. Panics on non-zero exit.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {}: {e}", args.join(" ")));

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("git {} failed: {stderr}", args.join(" "));
    }
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Set up a jj workspace with a bare git remote and trunk established.
///
/// Returns `(repo_dir, remote_dir)`. Both are temp dirs that the caller
/// should clean up.
fn setup_workspace_with_remote() -> (PathBuf, PathBuf) {
    let repo_dir = tempfile::tempdir().unwrap().keep();
    let remote_dir = tempfile::tempdir().unwrap().keep();

    // Create bare git remote
    git(&remote_dir, &["init", "--bare"]);

    // Create jj workspace (colocated with git)
    jj(&repo_dir, &["git", "init"]);

    // Add the bare repo as "origin"
    git(
        &repo_dir,
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
    );

    // Create an initial commit and push as main to establish trunk()
    jj(&repo_dir, &["describe", "-m", "initial trunk"]);
    jj(
        &repo_dir,
        &["bookmark", "create", "main", "-r", "@"],
    );
    jj(
        &repo_dir,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );

    // After push, @ became immutable and jj auto-created a new empty change.
    // Import git refs so trunk() resolves correctly.
    jj(&repo_dir, &["git", "import"]);

    (repo_dir, remote_dir)
}

/// Simulate a GitHub-style squash-merge of bookmark-a into trunk.
///
/// This creates a NEW commit directly on the remote's main branch (via
/// the bare git repo), then fetches it. This mirrors what GitHub does:
/// the squash-merge commit is a new commit with a different ID than
/// bookmark-a, even though it has the same content. After fetch, trunk()
/// points to this new commit, NOT to bookmark-a — forcing a real rebase
/// when we move bookmark-b onto trunk.
fn simulate_squash_merge(repo_dir: &Path, remote_dir: &Path) {
    // Clone the bare remote into a temp dir so we can commit directly to main
    let scratch = tempfile::tempdir().unwrap().keep();
    git(
        &scratch,
        &["clone", remote_dir.to_str().unwrap(), "."],
    );
    git(&scratch, &["checkout", "main"]);

    // Create a new "squash-merge" commit on main that is NOT the bookmark-a commit
    git(
        &scratch,
        &[
            "-c", "user.name=GitHub",
            "-c", "user.email=noreply@github.com",
            "commit", "--allow-empty",
            "-m", "squash-merge of bookmark-a (#1)",
        ],
    );
    git(&scratch, &["push", "origin", "main"]);

    // Back in the jj workspace, fetch so trunk() picks up the new commit.
    // Use the real jj to avoid going through the shim.
    jj(repo_dir, &["git", "fetch", "--remote", "origin"]);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// Create a 2-bookmark stack on top of the current working copy.
///
/// Stack shape: trunk ← bookmark-a ← bookmark-b ← @(empty)
fn create_two_bookmark_stack(repo_dir: &Path) {
    jj(repo_dir, &["new", "-m", "change for bookmark-a"]);
    jj(
        repo_dir,
        &["bookmark", "create", "bookmark-a", "-r", "@"],
    );

    jj(repo_dir, &["new", "-m", "change for bookmark-b"]);
    jj(
        repo_dir,
        &["bookmark", "create", "bookmark-b", "-r", "@"],
    );

    jj(
        repo_dir,
        &[
            "git",
            "push",
            "--remote",
            "origin",
            "--bookmark",
            "bookmark-a",
            "--bookmark",
            "bookmark-b",
            "--allow-empty-description",
        ],
    );
}

/// Create a 3-bookmark stack on top of the current working copy.
///
/// Stack shape: trunk ← bookmark-a ← bookmark-b ← bookmark-c ← @(empty)
fn create_three_bookmark_stack(repo_dir: &Path) {
    jj(repo_dir, &["new", "-m", "change for bookmark-a"]);
    jj(
        repo_dir,
        &["bookmark", "create", "bookmark-a", "-r", "@"],
    );

    jj(repo_dir, &["new", "-m", "change for bookmark-b"]);
    jj(
        repo_dir,
        &["bookmark", "create", "bookmark-b", "-r", "@"],
    );

    jj(repo_dir, &["new", "-m", "change for bookmark-c"]);
    jj(
        repo_dir,
        &["bookmark", "create", "bookmark-c", "-r", "@"],
    );

    jj(
        repo_dir,
        &[
            "git",
            "push",
            "--remote",
            "origin",
            "--bookmark",
            "bookmark-a",
            "--bookmark",
            "bookmark-b",
            "--bookmark",
            "bookmark-c",
            "--allow-empty-description",
        ],
    );
}

#[test]
fn rebase_bookmark_onto_trunk_after_squash_merge() {
    let (repo_dir, remote_dir) = setup_workspace_with_remote();
    create_two_bookmark_stack(&repo_dir);

    // Simulate a squash-merge: creates a NEW commit on trunk, so
    // bookmark-b's parent (bookmark-a) is no longer on trunk's lineage.
    // This forces move_commits to actually rewrite bookmark-b.
    simulate_squash_merge(&repo_dir, &remote_dir);

    // Open the workspace and call rebase_bookmark_onto_trunk.
    // Before the fix, this panicked with:
    //   "BUG: Descendants have not been rebased after the last rewrites."
    let mut workspace =
        jj_plan::workspace::Workspace::open(&repo_dir).expect("should open workspace");

    let result = workspace.rebase_bookmark_onto_trunk("bookmark-b");
    assert!(
        result.is_ok(),
        "rebase_bookmark_onto_trunk should succeed, got: {result:?}"
    );

    // Verify bookmark-b's parent is now trunk (main), not bookmark-a.
    let parent_of_b = jj(
        &repo_dir,
        &[
            "log",
            "-r",
            "bookmark-b-",
            "-T",
            "bookmarks",
            "--no-graph",
        ],
    );
    assert!(
        parent_of_b.contains("main"),
        "bookmark-b's parent should be main (trunk) after rebase, got: {parent_of_b:?}"
    );

    let _ = std::fs::remove_dir_all(&repo_dir);
    let _ = std::fs::remove_dir_all(&remote_dir);
}

#[test]
fn rebase_bookmark_with_descendants_after_squash_merge() {
    let (repo_dir, remote_dir) = setup_workspace_with_remote();
    create_three_bookmark_stack(&repo_dir);

    // Simulate squash-merge of bookmark-a
    simulate_squash_merge(&repo_dir, &remote_dir);

    let mut workspace =
        jj_plan::workspace::Workspace::open(&repo_dir).expect("should open workspace");

    // Rebase bookmark-b (and its descendant bookmark-c) onto trunk.
    // MoveCommitsTarget::Roots rebases the target and all descendants.
    let result = workspace.rebase_bookmark_onto_trunk("bookmark-b");
    assert!(
        result.is_ok(),
        "rebase with descendants should succeed, got: {result:?}"
    );

    // Verify bookmark-b is now a child of main
    let parent_of_b = jj(
        &repo_dir,
        &[
            "log",
            "-r",
            "bookmark-b-",
            "-T",
            "bookmarks",
            "--no-graph",
        ],
    );
    assert!(
        parent_of_b.contains("main"),
        "bookmark-b's parent should be main after rebase, got: {parent_of_b:?}"
    );

    // Verify bookmark-c is still a descendant of bookmark-b (chain preserved)
    let parent_of_c = jj(
        &repo_dir,
        &[
            "log",
            "-r",
            "bookmark-c-",
            "-T",
            "bookmarks",
            "--no-graph",
        ],
    );
    assert!(
        parent_of_c.contains("bookmark-b"),
        "bookmark-c's parent should still be bookmark-b after rebase, got: {parent_of_c:?}"
    );

    let _ = std::fs::remove_dir_all(&repo_dir);
    let _ = std::fs::remove_dir_all(&remote_dir);
}