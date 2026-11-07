//! `FileWatcher` against real inotify: in-place writes and rename-over writes of the file are seen, other files in
//! its directory are not.

use std::sync::mpsc;
use std::time::Duration;

use git::FileWatcher;

#[test]
fn sees_writes_and_renames_of_its_file_only() {
    let d = std::env::temp_dir().join(format!("dirigent-file-watch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("a.txt");
    std::fs::write(&f, "one\n").unwrap();
    let (tx, rx) = mpsc::channel();
    let _w = FileWatcher::start(&f, move || tx.send(()).unwrap()).unwrap();
    let seen = |what: &str| rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("{what}: not seen"));
    let quiet = |what: &str| assert!(rx.recv_timeout(Duration::from_millis(400)).is_err(), "{what}: reported");

    std::fs::write(&f, "two\n").unwrap();
    seen("in-place write");
    quiet("one debounced report per write");

    std::fs::write(d.join("other.txt"), "x").unwrap();
    quiet("another file");

    std::fs::write(d.join("a.txt.tmp"), "three\n").unwrap();
    std::fs::rename(d.join("a.txt.tmp"), &f).unwrap();
    seen("rename over it");

    std::fs::remove_file(&f).unwrap();
    seen("removed");
    std::fs::write(&f, "back\n").unwrap();
    seen("created again");
    let _ = std::fs::remove_dir_all(&d);
}
