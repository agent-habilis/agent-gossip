//! The C ABI. `include/mesh.h` is the hand-written declaration of everything
//! here — change one, change the other; `tests/ffi_smoke.rs` and the C programs
//! under `examples/mesh-pipe-c` are what catch a mismatch.
//!
//! Conventions, uniform across the surface: a pointer-returning call yields NULL
//! on failure, an `int` call returns `0`/`-1`, and the reason for any failure is
//! in [`mesh_last_error`] on the calling thread. Every entry point catches
//! unwinds — a Rust panic crossing into C is undefined behaviour.
#![expect(
    unsafe_code,
    reason = "this module *is* the C ABI: raw pointers from the foreign caller, and a boxed handle it owns"
)]

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int, c_long};
use std::panic::AssertUnwindSafe;
use std::sync::OnceLock;
use std::time::Duration;

use crate::pipe::{Inbound, Opts, Pipe};

/// Capacity of [`MeshFrame::nick`], including the NUL. A nickname is two short
/// words; 64 bytes is comfortably more than the engine can mint.
const NICK_CAP: usize = 64;

thread_local! {
    /// The reason for the most recent failure on this thread. Held as a
    /// `CString` so [`mesh_last_error`] can hand out a borrowed pointer; valid
    /// until this thread's next `mesh_*` call.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// The mesh selectors, mirroring `mesh_opts` in the header. String fields are
/// NUL-terminated C strings or NULL; `max_peers == 0` takes the engine default.
#[repr(C)]
#[derive(Debug)]
pub struct MeshOpts {
    pub mesh: *const c_char,
    pub topic: *const c_char,
    pub nick: *const c_char,
    pub is_public: c_int,
    pub mdns: c_int,
    pub dht: c_int,
    pub relay: c_int,
    pub max_peers: usize,
}

/// One received frame's metadata, mirroring `mesh_frame` in the header. The
/// payload itself lands in the caller's own buffer; `len` says how much.
#[repr(C)]
#[derive(Debug)]
pub struct MeshFrame {
    pub nick: [c_char; NICK_CAP],
    pub directed: c_int,
    pub eof: c_int,
    pub len: usize,
}

/// The opaque handle behind `mesh_pipe *`. Holds the [`Pipe`] plus NUL-terminated
/// copies of the identity strings, so [`mesh_id`] / [`mesh_nickname`] can hand
/// out pointers that stay valid for the handle's lifetime.
#[expect(
    missing_debug_implementations,
    reason = "wraps Pipe, which owns a tokio Runtime and so has no Debug impl"
)]
pub struct MeshPipe {
    pipe: Pipe,
    id: CString,
    nick: CString,
}

fn set_error(message: &str) {
    // A NUL inside the message would truncate it; replace rather than drop the
    // whole diagnostic.
    let sanitized = message.replace('\0', "\\0");
    let cstring = CString::new(sanitized).unwrap_or_else(|_| CString::default());
    LAST_ERROR.with_borrow_mut(|slot| *slot = Some(cstring));
}

fn clear_error() {
    LAST_ERROR.with_borrow_mut(|slot| *slot = None);
}

/// Run `body`, converting a panic into `fallback` plus an error message. Every
/// entry point goes through this: unwinding into C is undefined behaviour.
fn guard<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_panic) => {
            set_error("a panic crossed the FFI boundary");
            fallback
        }
    }
}

/// Borrow a caller-provided C string. `Ok(None)` for NULL (every optional field
/// spells "unset" that way); an error for non-UTF-8 bytes.
///
/// # Safety
/// `ptr` is NULL or points to a NUL-terminated string that outlives the call.
unsafe fn optional_str(ptr: *const c_char) -> Result<Option<&'static str>, ()> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: the caller guarantees a NUL-terminated string; the returned
    // lifetime is immediately narrowed by the callers, which only read it
    // during their own call.
    match unsafe { CStr::from_ptr(ptr) }.to_str() {
        Ok(text) => Ok(Some(text)),
        Err(error) => {
            set_error(&format!("argument is not valid UTF-8: {error}"));
            Err(())
        }
    }
}

/// Copy `text` into a caller buffer, NUL-terminating it. Returns the length
/// `text` needs (excluding the NUL); when that does not fit in `cap` nothing is
/// written and the caller retries with a bigger buffer.
///
/// # Safety
/// `buf` is NULL, or writable for `cap` bytes.
unsafe fn copy_out(text: &str, buf: *mut c_char, cap: usize) -> c_long {
    let bytes = text.as_bytes();
    let needed = bytes.len();
    if !buf.is_null() && needed < cap {
        // SAFETY: `needed + 1 <= cap` bytes are writable per the contract above,
        // and the source is a distinct Rust-owned allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), needed);
            buf.add(needed).write(0);
        }
    }
    // Lengths here are JSON documents bounded by the automerge doc / roster
    // size, never anywhere near `c_long::MAX`.
    c_long::try_from(needed).unwrap_or(c_long::MAX)
}

/// Fill `MeshFrame::nick`, truncating a pathologically long nickname rather than
/// overflowing the caller's fixed array.
fn write_nick(field: &mut [c_char; NICK_CAP], nick: &str) {
    let bytes = nick.as_bytes();
    let take = bytes.len().min(NICK_CAP - 1);
    // SAFETY: `take < NICK_CAP` elements are written into an array of that size,
    // from a distinct Rust-owned allocation.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), field.as_mut_ptr().cast::<u8>(), take);
    }
    field[take] = 0;
}

/// Create or join a mesh and return an owned handle, or NULL on failure.
///
/// # Safety
/// `opts` must point to a readable [`MeshOpts`] whose string fields are NULL or
/// NUL-terminated. The returned handle must be released with [`mesh_close`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_open(opts: *const MeshOpts) -> *mut MeshPipe {
    guard(std::ptr::null_mut(), || {
        clear_error();
        if opts.is_null() {
            set_error("mesh_open: opts is NULL");
            return std::ptr::null_mut();
        }
        // SAFETY: non-NULL and readable per the contract above.
        let opts = unsafe { &*opts };
        // SAFETY: same contract, applied to each optional string field.
        let strings = unsafe {
            (
                optional_str(opts.mesh),
                optional_str(opts.topic),
                optional_str(opts.nick),
            )
        };
        let (Ok(mesh), Ok(topic), Ok(nick)) = strings else {
            return std::ptr::null_mut();
        };
        let parsed = Opts {
            mesh: mesh.map(str::to_owned),
            topic: topic.map(str::to_owned),
            nick: nick.map(str::to_owned),
            public: opts.is_public != 0,
            mdns: opts.mdns != 0,
            dht: opts.dht != 0,
            relay: opts.relay != 0,
            max_peers: opts.max_peers,
        };
        match Pipe::open(&parsed) {
            Ok(pipe) => {
                // The nickname the engine settled on — not `nick` above, which is
                // only what the caller asked for (and may have been NULL).
                let assigned = CString::new(pipe.nickname()).unwrap_or_default();
                let id = CString::new(pipe.mesh_id()).unwrap_or_default();
                Box::into_raw(Box::new(MeshPipe {
                    pipe,
                    id,
                    nick: assigned,
                }))
            }
            Err(error) => {
                set_error(&format!("{error:#}"));
                std::ptr::null_mut()
            }
        }
    })
}

/// The mesh id of this mesh, borrowed for the handle's lifetime.
///
/// # Safety
/// `handle` must be a live handle from [`mesh_open`], or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_id(handle: *const MeshPipe) -> *const c_char {
    guard(std::ptr::null(), || {
        // SAFETY: live handle or NULL, per the contract above.
        match unsafe { handle.as_ref() } {
            Some(handle) => handle.id.as_ptr(),
            None => std::ptr::null(),
        }
    })
}

/// Our nickname in this mesh, borrowed for the handle's lifetime.
///
/// # Safety
/// `handle` must be a live handle from [`mesh_open`], or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_nickname(handle: *const MeshPipe) -> *const c_char {
    guard(std::ptr::null(), || {
        // SAFETY: live handle or NULL, per the contract above.
        match unsafe { handle.as_ref() } {
            Some(handle) => handle.nick.as_ptr(),
            None => std::ptr::null(),
        }
    })
}

/// Send `len` bytes as `pipe_data` frames — broadcast when `to` is NULL,
/// directed at that peer's nickname otherwise. Returns 0, or -1 on failure.
///
/// # Safety
/// `handle` must be live; `to` NULL or NUL-terminated; `buf` readable for `len`
/// bytes (it may be NULL when `len` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_send(
    handle: *mut MeshPipe,
    to: *const c_char,
    buf: *const u8,
    len: usize,
) -> c_int {
    guard(-1, || {
        clear_error();
        // SAFETY: live handle or NULL, per the contract above.
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            set_error("mesh_send: handle is NULL");
            return -1;
        };
        // SAFETY: same contract, applied to the addressee string.
        let Ok(to) = (unsafe { optional_str(to) }) else {
            return -1;
        };
        if len > 0 && buf.is_null() {
            set_error("mesh_send: buf is NULL with a non-zero len");
            return -1;
        }
        let bytes = if len == 0 {
            &[][..]
        } else {
            // SAFETY: readable for `len` bytes per the contract above; borrowed
            // only for the duration of this call.
            unsafe { std::slice::from_raw_parts(buf, len) }
        };
        report(handle.pipe.send(to, bytes))
    })
}

/// Send the end-of-stream marker (`pipe_eof`). Returns 0, or -1 on failure.
///
/// # Safety
/// `handle` must be live; `to` NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_send_eof(handle: *mut MeshPipe, to: *const c_char) -> c_int {
    guard(-1, || {
        clear_error();
        // SAFETY: live handle or NULL, per the contract above.
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            set_error("mesh_send_eof: handle is NULL");
            return -1;
        };
        // SAFETY: same contract, applied to the addressee string.
        let Ok(to) = (unsafe { optional_str(to) }) else {
            return -1;
        };
        report(handle.pipe.send_eof(to))
    })
}

/// Take the next inbound frame, waiting up to `timeout_ms`. Returns 1 with `out`
/// filled, 0 on timeout, or -1 on failure. A `pipe_eof` frame arrives as 1 with
/// `out->eof` set — EOF is not signalled through the return value.
///
/// # Safety
/// `handle` must be live and not used concurrently from another thread; `buf`
/// writable for `cap` bytes; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_recv(
    handle: *mut MeshPipe,
    buf: *mut u8,
    cap: usize,
    timeout_ms: c_int,
    out: *mut MeshFrame,
) -> c_long {
    guard(-1, || {
        clear_error();
        // SAFETY: live, exclusively-owned handle per the contract above.
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            set_error("mesh_recv: handle is NULL");
            return -1;
        };
        if out.is_null() {
            set_error("mesh_recv: out is NULL");
            return -1;
        }
        let timeout = Duration::from_millis(u64::try_from(timeout_ms.max(0)).unwrap_or(0));
        let frame = match handle.pipe.recv(timeout) {
            Ok(Some(frame)) => frame,
            Ok(None) => return 0,
            Err(error) => {
                set_error(&format!("{error:#}"));
                return -1;
            }
        };
        let Inbound {
            nick,
            directed,
            eof,
            bytes,
        } = frame;
        if bytes.len() > cap {
            set_error(&format!(
                "mesh_recv: buffer of {cap} bytes is too small for a {}-byte frame (see mesh_max_chunk)",
                bytes.len()
            ));
            return -1;
        }
        if !bytes.is_empty() {
            if buf.is_null() {
                set_error("mesh_recv: buf is NULL");
                return -1;
            }
            // SAFETY: writable for `cap >= bytes.len()` bytes per the contract
            // above, from a distinct Rust-owned allocation.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len()) }
        }
        let mut meta = MeshFrame {
            nick: [0; NICK_CAP],
            directed: c_int::from(directed),
            eof: c_int::from(eof),
            len: bytes.len(),
        };
        write_nick(&mut meta.nick, &nick);
        // SAFETY: non-NULL (checked) and writable per the contract above.
        unsafe { out.write(meta) }
        1
    })
}

/// Apply an RFC 7386 merge document (a JSON object) to the shared `state`
/// channel and gossip the change. Returns 0, or -1 on failure.
///
/// # Safety
/// `handle` must be live; `json` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_state_merge(handle: *mut MeshPipe, json: *const c_char) -> c_int {
    guard(-1, || {
        clear_error();
        // SAFETY: live handle or NULL, per the contract above.
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            set_error("mesh_state_merge: handle is NULL");
            return -1;
        };
        // SAFETY: same contract, applied to the merge document.
        let Ok(json) = (unsafe { optional_str(json) }) else {
            return -1;
        };
        let Some(json) = json else {
            set_error("mesh_state_merge: json is NULL");
            return -1;
        };
        report(handle.pipe.state_merge(json))
    })
}

/// Write the merged shared `state` document as JSON into `buf`. Returns the
/// length the document needs (excluding the NUL) — when that does not fit in
/// `cap`, nothing is written and the caller should retry with a bigger buffer.
/// -1 on failure.
///
/// # Safety
/// `handle` must be live; `buf` NULL or writable for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_state_json(
    handle: *mut MeshPipe,
    buf: *mut c_char,
    cap: usize,
) -> c_long {
    guard(-1, || {
        clear_error();
        // SAFETY: live handle or NULL, per the contract above.
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            set_error("mesh_state_json: handle is NULL");
            return -1;
        };
        match handle.pipe.state_json() {
            // SAFETY: `buf`/`cap` are the caller's buffer per the contract above.
            Ok(json) => unsafe { copy_out(&json, buf, cap) },
            Err(error) => {
                set_error(&format!("{error:#}"));
                -1
            }
        }
    })
}

/// Write the live peer roster as JSON into `buf`. Same length/`cap` convention
/// as [`mesh_state_json`].
///
/// # Safety
/// `handle` must be live; `buf` NULL or writable for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_peers_json(
    handle: *mut MeshPipe,
    buf: *mut c_char,
    cap: usize,
) -> c_long {
    guard(-1, || {
        clear_error();
        // SAFETY: live handle or NULL, per the contract above.
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            set_error("mesh_peers_json: handle is NULL");
            return -1;
        };
        match handle.pipe.peers_json() {
            // SAFETY: `buf`/`cap` are the caller's buffer per the contract above.
            Ok(json) => unsafe { copy_out(&json, buf, cap) },
            Err(error) => {
                set_error(&format!("{error:#}"));
                -1
            }
        }
    })
}

/// The number of peers **other than you** in the mesh right now; `0` means you
/// are alone. -1 on failure.
///
/// # Safety
/// `handle` must be a live handle from [`mesh_open`], or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_peer_count(handle: *mut MeshPipe) -> c_long {
    guard(-1, || {
        clear_error();
        // SAFETY: live handle or NULL, per the contract above.
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            set_error("mesh_peer_count: handle is NULL");
            return -1;
        };
        match handle.pipe.peer_count() {
            // A roster is bounded by the active-view cap; this cannot overflow.
            Ok(count) => c_long::try_from(count).unwrap_or(c_long::MAX),
            Err(error) => {
                set_error(&format!("{error:#}"));
                -1
            }
        }
    })
}

/// Leave the mesh and free the handle. Returns 0, or -1 if the event loop
/// reported an error on the way out; the handle is freed either way, so it must
/// not be used again.
///
/// # Safety
/// `handle` must be a live handle from [`mesh_open`] (or NULL) and must not be
/// used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mesh_close(handle: *mut MeshPipe) -> c_int {
    guard(-1, || {
        clear_error();
        if handle.is_null() {
            return 0;
        }
        // SAFETY: the handle came from `Box::into_raw` in `mesh_open` and the
        // caller promises not to use it again, so reclaiming ownership here is
        // sound.
        let mut owned = unsafe { Box::from_raw(handle) };
        report(owned.pipe.close())
    })
}

/// The reason for the most recent failure on this thread, or NULL if the last
/// call succeeded. Borrowed until this thread's next `mesh_*` call.
#[unsafe(no_mangle)]
pub extern "C" fn mesh_last_error() -> *const c_char {
    guard(std::ptr::null(), || {
        LAST_ERROR.with_borrow(|slot| match slot {
            Some(message) => message.as_ptr(),
            None => std::ptr::null(),
        })
    })
}

/// The engine's build version stamp. Borrowed for the process's lifetime.
#[unsafe(no_mangle)]
pub extern "C" fn mesh_version() -> *const c_char {
    static VERSION: OnceLock<CString> = OnceLock::new();
    guard(std::ptr::null(), || {
        VERSION
            .get_or_init(|| {
                CString::new(agent_habilis_mesh::VERSION).unwrap_or_else(|_| CString::default())
            })
            .as_ptr()
    })
}

/// The largest payload one frame carries, and so the smallest receive buffer
/// [`mesh_recv`] is guaranteed not to reject.
#[unsafe(no_mangle)]
pub extern "C" fn mesh_max_chunk() -> usize {
    guard(0, crate::pipe::default_chunk)
}

/// Collapse a fallible operation into the `0` / `-1` return code, recording the
/// reason for a foreign caller that has no other way to see it.
fn report(outcome: anyhow::Result<()>) -> c_int {
    match outcome {
        Ok(()) => 0,
        Err(error) => {
            set_error(&format!("{error:#}"));
            -1
        }
    }
}
