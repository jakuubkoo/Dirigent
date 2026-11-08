//! Windows Recycle Bin edge cases for `write::trash`: a volume whose bin was never used yet (a fresh VHD), several
//! trashes at once on such a volume, a `subst` drive, and a share without a bin. The VHD and subst tests attach
//! drives (admin, diskpart), so they only run with `DIRIGENT_TEST_VHD=1` (the `windows` CI job). COM on reused and
//! already-initialized threads runs everywhere.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};

use git::write;

fn enabled() -> bool {
    std::env::var_os("DIRIGENT_TEST_VHD").is_some_and(|v| v == "1")
}

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("dirigent-recycle-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Names (without extension: the shell hides known ones) the Recycle Bin lists as deleted from `dir`.
fn recycled(dir: &Path) -> Vec<String> {
    let dir = git::plain_path(std::path::absolute(dir).unwrap());
    let script = format!(
        "(New-Object -ComObject Shell.Application).Namespace(10).Items() | \
         Where-Object {{ $_.ExtendedProperty('System.Recycle.DeletedFrom') -eq '{}' }} | ForEach-Object {{ $_.Name }}",
        dir.display()
    );
    let out = Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| Path::new(l.trim()).file_stem().unwrap_or_default().to_string_lossy().into_owned())
        .collect()
}

fn letters() -> Vec<char> {
    ('A'..='Z').filter(|c| Path::new(&format!("{c}:\\")).exists()).collect()
}

fn free_letter() -> char {
    ('M'..='Z').rev().find(|c| !Path::new(&format!("{c}:\\")).exists()).expect("a free drive letter")
}

/// Tests that attach drives run one at a time: each picks a free letter, and diskpart runs one script at a time.
static DRIVES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn drives() -> std::sync::MutexGuard<'static, ()> {
    DRIVES.lock().unwrap_or_else(|e| e.into_inner())
}

fn diskpart(dir: &Path, script: &str) -> String {
    let file = dir.join("diskpart.txt");
    std::fs::write(&file, script).unwrap();
    let out = Command::new("diskpart").arg("/s").arg(&file).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "diskpart failed:\n{script}\n{text}");
    text
}

/// A freshly formatted NTFS volume (a 64 MB VHD) at a free letter: nothing was ever recycled there. Detached on drop.
struct FreshVolume {
    vhd: PathBuf,
    root: PathBuf,
}

impl FreshVolume {
    fn new(name: &str) -> Self {
        let dir = tmp(name);
        let vhd = dir.join("fresh.vhdx");
        let before = letters();
        diskpart(
            &dir,
            &format!(
                "create vdisk file=\"{}\" maximum=64 type=expandable\nselect vdisk file=\"{}\"\nattach vdisk\n\
                 create partition primary\nformat fs=ntfs quick label=dirigent\nassign\n",
                vhd.display(),
                vhd.display()
            ),
        );
        // The letter Windows gave it (automount or `assign`): the one that wasn't there before.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let letter = loop {
            if let Some(c) = letters().into_iter().find(|c| !before.contains(c)) {
                break c;
            }
            assert!(std::time::Instant::now() < end, "the VHD's volume never appeared");
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        let root = PathBuf::from(format!("{letter}:\\"));
        assert!(!root.join("$Recycle.Bin").exists(), "a fresh volume has no bin folder yet");
        Self { vhd, root }
    }
}

impl Drop for FreshVolume {
    fn drop(&mut self) {
        let _ = std::panic::catch_unwind(|| {
            diskpart(
                self.vhd.parent().unwrap(),
                &format!("select vdisk file=\"{}\"\ndetach vdisk\n", self.vhd.display()),
            )
        });
    }
}

#[test]
fn first_trash_on_a_fresh_volume_is_recycled() {
    if !enabled() {
        return;
    }
    let _drives = drives();
    let v = FreshVolume::new("fresh");
    let dir = v.root.join("work");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("first.txt"), "x").unwrap();
    write::trash(&dir.join("first.txt")).unwrap();
    assert!(!dir.join("first.txt").exists());
    assert!(recycled(&dir).contains(&"first".to_string()), "{:?}", recycled(&dir));
}

#[test]
fn concurrent_trashes_on_a_fresh_volume_all_recycle() {
    if !enabled() {
        return;
    }
    let _drives = drives();
    let v = FreshVolume::new("concurrent");
    let dir = v.root.join("work");
    std::fs::create_dir_all(&dir).unwrap();
    const N: usize = 8;
    let start = Arc::new(Barrier::new(N));
    let results: Vec<_> = (0..N)
        .map(|i| {
            let (dir, start) = (dir.clone(), start.clone());
            std::thread::spawn(move || {
                let f = dir.join(format!("c{i}.txt"));
                std::fs::write(&f, "x").unwrap();
                start.wait();
                write::trash(&f).map_err(|e| e.to_string())
            })
        })
        .collect::<Vec<_>>() // all spawned before the first join: they wait for each other at the barrier
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    assert!(results.iter().all(Result::is_ok), "every trash succeeded: {results:?}");
    let listed = recycled(&dir);
    for i in 0..N {
        assert!(listed.contains(&format!("c{i}")), "c{i} in the bin: {listed:?}");
    }
}

#[test]
fn subst_drive_trash_is_recycled() {
    if !enabled() {
        return;
    }
    let _drives = drives();
    let target = tmp("subst");
    let letter = free_letter();
    let drive = format!("{letter}:");
    assert!(Command::new("subst").arg(&drive).arg(&target).status().unwrap().success());
    let root = PathBuf::from(format!("{drive}\\"));
    std::fs::write(root.join("s.txt"), "x").unwrap();
    let res = write::trash(&root.join("s.txt"));
    let _ = Command::new("subst").arg(&drive).arg("/d").status();
    res.unwrap();
    assert!(!target.join("s.txt").exists());
}

#[test]
fn a_share_without_a_bin_keeps_the_file() {
    if !enabled() {
        return;
    }
    // The admin share of this machine's temp dir: a network path, which has no Recycle Bin.
    let local = tmp("share");
    let abs = std::path::absolute(&local).unwrap();
    let s = abs.to_string_lossy();
    let (drive, rest) = s.split_once(":\\").unwrap();
    let unc = PathBuf::from(format!(r"\\localhost\{drive}$\{rest}"));
    std::fs::write(local.join("kept.txt"), "x").unwrap();
    assert!(unc.join("kept.txt").exists(), "the admin share is reachable: {}", unc.display());
    let e = write::trash(&unc.join("kept.txt")).unwrap_err().to_string();
    assert!(e.contains("no Recycle Bin"), "refused by the sink, not some other failure: {e}");
    assert!(local.join("kept.txt").exists(), "the file is kept");
}

/// Discards run on reused background threads: COM set up by one trash must not break the next on that thread.
#[test]
fn two_trashes_in_a_row_on_one_thread() {
    let dir = tmp("same-thread");
    std::thread::spawn(move || {
        for name in ["one", "two"] {
            std::fs::write(dir.join(format!("{name}.txt")), "x").unwrap();
            write::trash(&dir.join(format!("{name}.txt"))).unwrap();
        }
        let listed = recycled(&dir);
        assert!(listed.contains(&"one".into()) && listed.contains(&"two".into()), "{listed:?}");
    })
    .join()
    .unwrap();
}

/// A thread whose COM is already multithreaded (someone else's CoInitializeEx): trash still works and leaves that
/// initialization alone.
#[test]
fn trash_on_a_thread_already_in_the_mta() {
    use windows_sys::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    let dir = tmp("mta");
    std::thread::spawn(move || {
        // SAFETY: plain COM init/uninit on this thread, balanced below.
        assert!(unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) } >= 0);
        for name in ["m1", "m2"] {
            std::fs::write(dir.join(format!("{name}.txt")), "x").unwrap();
            write::trash(&dir.join(format!("{name}.txt"))).unwrap();
        }
        // Still in the MTA: a second MTA init is S_FALSE (already), not RPC_E_CHANGED_MODE.
        // SAFETY: as above.
        assert_eq!(unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) }, 1);
        // SAFETY: balances the two inits above.
        unsafe { (CoUninitialize(), CoUninitialize()) };
        assert!(recycled(&dir).contains(&"m2".into()));
    })
    .join()
    .unwrap();
}
