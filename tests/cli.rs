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
