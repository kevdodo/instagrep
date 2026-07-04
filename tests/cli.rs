use std::fs;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_instagrep")
}

#[test]
fn builds_index_and_searches_with_grep_output() {
    let dir = tempfile::tempdir().unwrap();
    let hello = dir.path().join("hello.txt");
    let other = dir.path().join("other.txt");
    fs::write(&hello, "hello world\nfoo bar baz\n").unwrap();
    fs::write(&other, "world of code\nhello again\n").unwrap();

    let build = Command::new(bin())
        .arg("--build")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(dir.path().join(".instantgrep/index.bin").is_file());

    let search = Command::new(bin())
        .arg("hello")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    let stdout = String::from_utf8(search.stdout).unwrap();
    assert!(stdout.contains(&format!("{}:1:hello world", hello.display())));
    assert!(stdout.contains(&format!("{}:2:hello again", other.display())));
}

#[test]
fn ignore_case_indexed_search_finds_uppercase_content() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "TODO: ship the Rust port\n").unwrap();

    let build = Command::new(bin())
        .arg("--build")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let search = Command::new(bin())
        .arg("-i")
        .arg("todo")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    let stdout = String::from_utf8(search.stdout).unwrap();
    assert!(stdout.contains(&format!("{}:1:TODO: ship the Rust port", file.display())));
}

#[test]
fn no_index_mode_does_not_create_index() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    fs::write(&file, "fn main() {}\n").unwrap();

    let search = Command::new(bin())
        .arg("--no-index")
        .arg("main")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    assert!(!dir.path().join(".instantgrep").exists());
    assert!(
        String::from_utf8(search.stdout)
            .unwrap()
            .contains(&format!("{}:1:fn main() {{}}", file.display()))
    );
}

/// Regression: files containing invalid UTF-8 must still be searched. The
/// previous implementation used `read_to_string` and silently dropped them.
#[test]
fn non_utf8_file_is_still_searched() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bin.dat");
    let mut contents = b"line one\nneedle here".to_vec();
    contents.extend_from_slice(b"\xff\xfe bad bytes\nline three\n"); // invalid UTF-8
    fs::write(&file, contents).unwrap();

    Command::new(bin()).arg("--build").arg(dir.path()).output().unwrap();

    let search = Command::new(bin()).arg("needle").arg(dir.path()).output().unwrap();
    let stdout = String::from_utf8(search.stdout).unwrap();
    assert!(
        stdout.contains(&format!("{}:2:", file.display())),
        "expected a match in the non-UTF-8 file, got: {stdout}"
    );
}

#[test]
fn count_flag_reports_per_file_counts() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "todo: a\ntodo: b\nnothing here\ntodo: c\n").unwrap();
    Command::new(bin()).arg("--build").arg(dir.path()).output().unwrap();

    let search = Command::new(bin()).args(["-c", "todo", dir.path().to_str().unwrap()]).output().unwrap();
    let stdout = String::from_utf8(search.stdout).unwrap();
    assert!(stdout.contains(&format!("{}:3", file.display())), "got: {stdout}");
}

#[test]
fn files_with_matches_flag_lists_paths_only() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "hello world\n").unwrap();
    fs::write(&b, "goodbye\n").unwrap();
    Command::new(bin()).arg("--build").arg(dir.path()).output().unwrap();

    let search = Command::new(bin()).args(["-l", "hello", dir.path().to_str().unwrap()]).output().unwrap();
    let stdout = String::from_utf8(search.stdout).unwrap();
    assert!(stdout.contains(a.to_str().unwrap()), "got: {stdout}");
    assert!(!stdout.contains(b.to_str().unwrap()), "got: {stdout}");
}

#[test]
fn context_flag_includes_surrounding_lines() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ctx.txt");
    fs::write(&file, "alpha\nBETA\ngamma\n").unwrap();
    Command::new(bin()).arg("--build").arg(dir.path()).output().unwrap();

    let search = Command::new(bin())
        .args(["-C", "1", "BETA", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8(search.stdout).unwrap();
    // Match line plus one line of context on each side.
    assert!(stdout.contains(&format!("{}:2:BETA", file.display())), "match line missing: {stdout}");
    assert!(stdout.contains(&format!("{}-1-alpha", file.display())), "before context missing: {stdout}");
    assert!(stdout.contains(&format!("{}-3-gamma", file.display())), "after context missing: {stdout}");
}

#[test]
fn incremental_update_makes_new_files_searchable() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "existing content\n").unwrap();
    Command::new(bin()).arg("--build").arg(dir.path()).output().unwrap();

    fs::write(dir.path().join("b.txt"), "freshly added content\n").unwrap();
    let update = Command::new(bin()).arg("--update").arg(dir.path()).output().unwrap();
    assert!(update.status.success(), "{}", String::from_utf8_lossy(&update.stderr));

    let search = Command::new(bin()).arg("freshly").arg(dir.path()).output().unwrap();
    let stdout = String::from_utf8(search.stdout).unwrap();
    assert!(stdout.contains("b.txt"), "new file not found after update: {stdout}");
}
