//! Windows: moving one item to the Recycle Bin with `IFileOperation`, never deleting it. A progress sink aborts the
//! delete when the shell says it can't be recycled (no bin on that drive, too big for the bin), the way Electron's
//! `shell.trashItem` does. No `windows` crate: the two interfaces are called through hand-written vtables.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{E_NOINTERFACE, E_POINTER, RPC_E_CHANGED_MODE, S_OK};
use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;
use windows_sys::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NO_UI, FOFX_RECYCLEONDELETE, FileOperation, SHCreateItemFromParsingName,
    TSF_DELETE_RECYCLE_IF_POSSIBLE,
};
use windows_sys::core::{BOOL, GUID, HRESULT};

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_IFILEOPERATION: GUID = GUID::from_u128(0x947aab5f_0a5c_4c13_b4d6_4bf7836fc9f8);
const IID_IFILEOPERATIONPROGRESSSINK: GUID = GUID::from_u128(0x04b0f1a7_9490_44bc_96e1_4296a31252e2);
const IID_ISHELLITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);

/// Why an item wasn't recycled.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NotRecycled {
    /// The shell would have deleted it outright (no bin on the drive, or too big for it); nothing was touched.
    NoBin,
    /// Something else stopped it (e.g. the user cancelled a shell prompt).
    Aborted,
    /// A COM or shell call failed with this HRESULT.
    Failed(HRESULT),
}

/// Moves `path` (absolute, no `\\?\`) to the Recycle Bin. Blocking; `owner` (an HWND) owns any shell window.
pub(crate) fn recycle(path: &Path, owner: Option<isize>) -> Result<(), NotRecycled> {
    let _com = Com::init()?; // declared first: dropped last, after every interface below is released
    let wide: Vec<u16> = unsubst(path).as_os_str().encode_wide().chain([0]).collect();
    let mut item = Unknown::null();
    // SAFETY: a NUL-terminated path; `item` receives an IShellItem.
    check(unsafe { SHCreateItemFromParsingName(wide.as_ptr(), std::ptr::null_mut(), &IID_ISHELLITEM, &mut item.0) })?;
    let mut op = Unknown::null();
    // SAFETY: standard activation; `op` receives an IFileOperation.
    check(unsafe {
        CoCreateInstance(&FileOperation, std::ptr::null_mut(), CLSCTX_ALL, &IID_IFILEOPERATION, &mut op.0)
    })?;
    let sink = Unknown(Sink::create());
    // SAFETY: `op` is an IFileOperation (vtable indices from shobjidl_core.h), `item` an IShellItem and `sink` our
    // IFileOperationProgressSink, all alive for these calls.
    unsafe {
        let v = *(op.0 as *const *const FileOperationVtbl);
        check(((*v).set_operation_flags)(op.0, FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE))?;
        if let Some(hwnd) = owner {
            check(((*v).set_owner_window)(op.0, hwnd as *mut c_void))?;
        }
        check(((*v).delete_item)(op.0, item.0, sink.0))?;
        let hr = ((*v).perform_operations)(op.0);
        if (*(sink.0 as *const Sink)).refused.load(Ordering::SeqCst) {
            return Err(NotRecycled::NoBin);
        }
        check(hr)?;
        let mut aborted: BOOL = 0;
        check(((*v).get_any_operations_aborted)(op.0, &mut aborted))?;
        if aborted != 0 {
            return Err(NotRecycled::Aborted);
        }
    }
    Ok(())
}

/// `path` on a `subst` drive as the folder that drive stands for (nested substs too, up to 8 levels): the shell
/// recycles nothing on a subst drive, but does on the volume behind it. Any other drive (a volume, a mapped network
/// drive) stays as it is.
fn unsubst(path: &Path) -> PathBuf {
    unsubst_with(path, dos_device)
}

/// `unsubst` with `dos` (a drive letter's DOS device target) pluggable, for tests.
fn unsubst_with(path: &Path, dos: impl Fn(u8) -> Option<String>) -> PathBuf {
    let mut p = path.to_path_buf();
    for _ in 0..8 {
        let mut parts = p.components();
        let Some(Component::Prefix(prefix)) = parts.next() else { break };
        let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else { break };
        let Some(target) = dos(letter).as_deref().and_then(subst_target) else { break };
        let rest: PathBuf = parts.filter(|c| !matches!(c, Component::RootDir)).collect();
        p = target.join(rest);
    }
    p
}

/// What drive `letter:` maps to in the DOS device namespace (`\??\C:\dir` for a subst drive).
fn dos_device(letter: u8) -> Option<String> {
    let name = [letter as u16, b':' as u16, 0];
    let mut buf = [0u16; 1024];
    // SAFETY: a NUL-terminated name and a buffer of the length passed.
    let n = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let first = &buf[..n.min(buf.len())];
    let end = first.iter().position(|&c| c == 0).unwrap_or(first.len());
    (end > 0).then(|| String::from_utf16(&first[..end]).ok()).flatten()
}

/// The folder of a subst mapping (`\??\<drive>:` with an optional path); `None` for anything else.
fn subst_target(dos: &str) -> Option<PathBuf> {
    let rest = dos.strip_prefix(r"\??\")?;
    let b = rest.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\');
    drive.then(|| PathBuf::from(if b.len() == 2 { format!("{rest}\\") } else { rest.to_owned() }))
}

fn check(hr: HRESULT) -> Result<(), NotRecycled> {
    if hr < 0 { Err(NotRecycled::Failed(hr)) } else { Ok(()) }
}

/// COM on the calling thread (a reused background thread): STA if we start it, balanced on drop. A thread already in
/// another apartment (`RPC_E_CHANGED_MODE`) is used as it is and left alone.
struct Com(bool);

impl Com {
    fn init() -> Result<Self, NotRecycled> {
        // SAFETY: plain per-thread COM initialization.
        let hr =
            unsafe { CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
        match hr {
            RPC_E_CHANGED_MODE => Ok(Com(false)),
            hr if hr < 0 => Err(NotRecycled::Failed(hr)),
            _ => Ok(Com(true)), // S_OK, or S_FALSE (already STA): both need their CoUninitialize
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances our successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// An owned COM reference, released on drop.
struct Unknown(*mut c_void);

impl Unknown {
    fn null() -> Self {
        Self(std::ptr::null_mut())
    }
}

impl Drop for Unknown {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a live COM object; every vtable starts with IUnknown.
            unsafe { ((**(self.0 as *const *const UnknownVtbl)).release)(self.0) };
        }
    }
}

type Raw = *mut c_void;

#[repr(C)]
struct UnknownVtbl {
    query_interface: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> HRESULT,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
}

/// IFileOperation: only the slots we call are typed.
#[repr(C)]
struct FileOperationVtbl {
    unknown: UnknownVtbl,
    _advise_to_set_operation_flags: [usize; 2],
    set_operation_flags: unsafe extern "system" fn(Raw, u32) -> HRESULT,
    _set_progress_message_to_set_properties: [usize; 3],
    set_owner_window: unsafe extern "system" fn(Raw, *mut c_void) -> HRESULT,
    _apply_properties_to_item_to_copy_items: [usize; 8],
    delete_item: unsafe extern "system" fn(Raw, Raw, Raw) -> HRESULT,
    _delete_items_new_item: [usize; 2],
    perform_operations: unsafe extern "system" fn(Raw) -> HRESULT,
    get_any_operations_aborted: unsafe extern "system" fn(Raw, *mut BOOL) -> HRESULT,
}

/// IFileOperationProgressSink, in shobjidl_core.h order.
#[repr(C)]
struct SinkVtbl {
    unknown: UnknownVtbl,
    start_operations: unsafe extern "system" fn(Raw) -> HRESULT,
    finish_operations: unsafe extern "system" fn(Raw, HRESULT) -> HRESULT,
    pre_rename_item: unsafe extern "system" fn(Raw, u32, Raw, *const u16) -> HRESULT,
    post_rename_item: unsafe extern "system" fn(Raw, u32, Raw, *const u16, HRESULT, Raw) -> HRESULT,
    pre_move_item: unsafe extern "system" fn(Raw, u32, Raw, Raw, *const u16) -> HRESULT,
    post_move_item: unsafe extern "system" fn(Raw, u32, Raw, Raw, *const u16, HRESULT, Raw) -> HRESULT,
    pre_copy_item: unsafe extern "system" fn(Raw, u32, Raw, Raw, *const u16) -> HRESULT,
    post_copy_item: unsafe extern "system" fn(Raw, u32, Raw, Raw, *const u16, HRESULT, Raw) -> HRESULT,
    pre_delete_item: unsafe extern "system" fn(Raw, u32, Raw) -> HRESULT,
    post_delete_item: unsafe extern "system" fn(Raw, u32, Raw, HRESULT, Raw) -> HRESULT,
    pre_new_item: unsafe extern "system" fn(Raw, u32, Raw, *const u16) -> HRESULT,
    post_new_item: unsafe extern "system" fn(Raw, u32, Raw, *const u16, *const u16, u32, HRESULT, Raw) -> HRESULT,
    update_progress: unsafe extern "system" fn(Raw, u32, u32) -> HRESULT,
    reset_timer: unsafe extern "system" fn(Raw) -> HRESULT,
    pause_timer: unsafe extern "system" fn(Raw) -> HRESULT,
    resume_timer: unsafe extern "system" fn(Raw) -> HRESULT,
}

/// Our progress sink: a heap COM object (refcounted, freed on the last Release). Atomics: from another apartment
/// the shell calls it on RPC threads.
#[repr(C)]
struct Sink {
    vtbl: &'static SinkVtbl,
    refs: AtomicU32,
    /// Set when the shell was about to delete something it couldn't recycle (and we aborted that).
    refused: AtomicBool,
}

impl Sink {
    /// A new sink with one reference, owned by the caller.
    fn create() -> Raw {
        Box::into_raw(Box::new(Sink { vtbl: &SINK_VTBL, refs: AtomicU32::new(1), refused: AtomicBool::new(false) }))
            .cast()
    }
}

unsafe extern "system" fn query_interface(this: Raw, iid: *const GUID, out: *mut Raw) -> HRESULT {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM passes a valid IID and out pointer.
    let iid = unsafe { &*iid };
    let ok = [IID_IUNKNOWN, IID_IFILEOPERATIONPROGRESSSINK]
        .iter()
        .any(|g| (g.data1, g.data2, g.data3, g.data4) == (iid.data1, iid.data2, iid.data3, iid.data4));
    // SAFETY: as above; `this` is a live Sink.
    unsafe {
        if ok {
            add_ref(this);
            *out = this;
            S_OK
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn add_ref(this: Raw) -> u32 {
    // SAFETY: `this` is a live Sink.
    unsafe { (*(this as *const Sink)).refs.fetch_add(1, Ordering::Relaxed) + 1 }
}

unsafe extern "system" fn release(this: Raw) -> u32 {
    // SAFETY: `this` is a live Sink until its last reference goes; then it was made by `Box::into_raw` in `new`.
    unsafe {
        let left = (*(this as *const Sink)).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(this as *mut Sink));
        }
        left
    }
}

unsafe extern "system" fn pre_delete_item(this: Raw, flags: u32, _item: Raw) -> HRESULT {
    if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE as u32 != 0 {
        return S_OK;
    }
    // SAFETY: `this` is a live Sink.
    unsafe { (*(this as *const Sink)).refused.store(true, Ordering::SeqCst) };
    windows_sys::Win32::Foundation::E_ABORT
}

unsafe extern "system" fn ok0(_: Raw) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_hr(_: Raw, _: HRESULT) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_rename(_: Raw, _: u32, _: Raw, _: *const u16) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_post_rename(_: Raw, _: u32, _: Raw, _: *const u16, _: HRESULT, _: Raw) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_move(_: Raw, _: u32, _: Raw, _: Raw, _: *const u16) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_post_move(_: Raw, _: u32, _: Raw, _: Raw, _: *const u16, _: HRESULT, _: Raw) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_post_delete(_: Raw, _: u32, _: Raw, _: HRESULT, _: Raw) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_post_new(
    _: Raw,
    _: u32,
    _: Raw,
    _: *const u16,
    _: *const u16,
    _: u32,
    _: HRESULT,
    _: Raw,
) -> HRESULT {
    S_OK
}
unsafe extern "system" fn ok_progress(_: Raw, _: u32, _: u32) -> HRESULT {
    S_OK
}

static SINK_VTBL: SinkVtbl = SinkVtbl {
    unknown: UnknownVtbl { query_interface, add_ref, release },
    start_operations: ok0,
    finish_operations: ok_hr,
    pre_rename_item: ok_rename,
    post_rename_item: ok_post_rename,
    pre_move_item: ok_move,
    post_move_item: ok_post_move,
    pre_copy_item: ok_move,
    post_copy_item: ok_post_move,
    pre_delete_item,
    post_delete_item: ok_post_delete,
    pre_new_item: ok_rename,
    post_new_item: ok_post_new,
    update_progress: ok_progress,
    reset_timer: ok0,
    pause_timer: ok0,
    resume_timer: ok0,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn vtbl(p: Raw) -> &'static UnknownVtbl {
        // SAFETY: `p` is a live Sink.
        unsafe { &(*(p as *const Sink)).vtbl.unknown }
    }

    #[test]
    fn subst_targets() {
        assert_eq!(subst_target(r"\??\C:\work\dir"), Some(PathBuf::from(r"C:\work\dir")));
        assert_eq!(subst_target(r"\??\d:"), Some(PathBuf::from(r"d:\")));
        assert_eq!(subst_target(r"\Device\HarddiskVolume3"), None);
        assert_eq!(subst_target(r"\Device\LanmanRedirector\;Z:0000000000012345\srv\share"), None);
        assert_eq!(subst_target(r"\??\UNC\srv\share"), None);
        assert_eq!(subst_target(r"\??\CD:\x"), None);
    }

    #[test]
    fn unsubst_follows_nested_and_verbatim_drives() {
        let dos = |l: u8| match l {
            b'Z' => Some(r"\??\Y:\inner".to_owned()),
            b'Y' => Some(r"\??\C:\work".to_owned()),
            b'N' => Some(r"\Device\LanmanRedirector\;N:0000000000012345\srv\share".to_owned()),
            b'L' => Some(r"\??\L:\".to_owned()), // a loop: stops after 8 steps
            _ => Some(r"\Device\HarddiskVolume3".to_owned()),
        };
        let real = PathBuf::from(r"C:\work\inner\a\b.txt");
        assert_eq!(unsubst_with(Path::new(r"Z:\a\b.txt"), dos), real);
        assert_eq!(unsubst_with(Path::new(r"\\?\Z:\a\b.txt"), dos), real);
        assert_eq!(unsubst_with(Path::new(r"Y:\"), dos), PathBuf::from(r"C:\work"));
        for kept in [r"C:\x\y.txt", r"N:\x.txt", r"\\srv\share\x.txt", r"\\?\UNC\srv\share\x.txt"] {
            assert_eq!(unsubst_with(Path::new(kept), dos), PathBuf::from(kept));
        }
        assert_eq!(unsubst_with(Path::new(r"L:\x.txt"), dos), PathBuf::from(r"L:\x.txt"));
    }

    #[test]
    fn sink_refcount_and_interfaces() {
        let s = Sink::create();
        let v = vtbl(s);
        // SAFETY: `s` stays alive until the last Release below.
        unsafe {
            assert_eq!((v.add_ref)(s), 2);
            let mut out: Raw = std::ptr::null_mut();
            assert_eq!((v.query_interface)(s, &IID_IFILEOPERATIONPROGRESSSINK, &mut out), S_OK);
            assert_eq!(out, s);
            assert_eq!((v.query_interface)(s, &IID_IUNKNOWN, &mut out), S_OK);
            assert_eq!((*(s as *const Sink)).refs.load(Ordering::SeqCst), 4, "each QI took a reference");
            assert_eq!((v.query_interface)(s, &IID_ISHELLITEM, &mut out), E_NOINTERFACE);
            assert!(out.is_null());
            assert_eq!((v.query_interface)(s, &IID_IUNKNOWN, std::ptr::null_mut()), E_POINTER);
            assert_eq!([(v.release)(s), (v.release)(s), (v.release)(s)], [3, 2, 1]);
            assert_eq!((v.release)(s), 0, "the last Release frees it");
        }
    }

    #[test]
    fn pre_delete_refuses_unless_recyclable() {
        let s = Sink::create();
        // SAFETY: `s` is a live Sink, released at the end.
        unsafe {
            let sink = &*(s as *const Sink);
            assert_eq!(pre_delete_item(s, TSF_DELETE_RECYCLE_IF_POSSIBLE as u32, std::ptr::null_mut()), S_OK);
            assert!(!sink.refused.load(Ordering::SeqCst));
            assert!(pre_delete_item(s, 0, std::ptr::null_mut()) < 0, "would be deleted outright: aborted");
            assert!(sink.refused.load(Ordering::SeqCst));
            assert_eq!((vtbl(s).release)(s), 0);
        }
    }
}
