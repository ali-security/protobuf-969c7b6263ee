// Protocol Buffers - Google's data interchange format
// Copyright 2026 Google LLC.  All rights reserved.
//
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file or at
// https://developers.google.com/open-source/licenses/bsd

//! The process-wide `DefPool` that reflection on the upb kernel reads from.
//!
//! Generated code gives each .proto file a `def_init()` function that returns the file's
//! `upb_DefPool_Init`, built with [`build_def_init`]. It also implements [`UpbWithReflection`]
//! for each message, so that [`message_def`] can get from a message type to that init.

use super::{MiniTableEnumPtr, MiniTableExtensionPtr, MiniTablePtr, THREAD_LOCAL_ARENA};
use std::ffi::CStr;
use std::sync::{Mutex, OnceLock};

// `reflection` here is `//third_party/upb/rust:reflection`, the safe wrappers over upb's
// def pool -- not this crate's own reflection support.
use reflection::{build_def_pool_init, build_mini_table_file, DefPool, DefPoolInitPtr, MessageDef};

/// A message whose `MessageDef` can be looked up with [`message_def`]. Implemented by
/// generated code.
///
/// # Safety
/// - `FULL_NAME` must be the fully-qualified name of the message.
/// - `def_init` must return the init of the .proto file that declares the message.
pub unsafe trait UpbWithReflection {
    /// The fully-qualified name of the message, e.g. `"my.pkg.Outer.Inner"`.
    const FULL_NAME: &'static str;

    /// Returns the `upb_DefPool_Init` of the .proto file that declares the message.
    fn def_init() -> DefPoolInit;
}

/// A `upb_DefPool_Init` built by [`build_def_init`].
#[derive(Clone, Copy)]
pub struct DefPoolInit(DefPoolInitPtr);

// SAFETY: An init is never written to or freed after `build_def_init` returns it, so any
// thread can read it.
unsafe impl Send for DefPoolInit {}
// SAFETY: As above.
unsafe impl Sync for DefPoolInit {}

/// Builds the `upb_DefPool_Init` for a .proto file. For generated code only.
///
/// # Safety
/// - `filename` and `descriptor` must be the file's name and its serialized `FileDescriptorProto`.
/// - `deps` must be the inits of the files it imports.
/// - The MiniTables must be the file's own, in the order [`build_mini_table_file`] documents.
pub unsafe fn build_def_init(
    filename: &'static CStr,
    descriptor: &'static [u8],
    deps: &[DefPoolInit],
    msgs: &[MiniTablePtr],
    enums: &[MiniTableEnumPtr],
    exts: &[MiniTableExtensionPtr],
) -> DefPoolInit {
    let deps: Vec<DefPoolInitPtr> = deps.iter().map(|dep| dep.0).collect();
    THREAD_LOCAL_ARENA.with(|arena| {
        // SAFETY:
        // - The arguments are valid per the safety requirements of this function.
        // - upb borrows the layout, the init and the arrays they point at rather than copying them,
        //   so they have to outlive the global pool. `THREAD_LOCAL_ARENA` is never freed, and
        //   neither are the MiniTables, `filename` and `descriptor`.
        unsafe {
            let layout = build_mini_table_file(arena, msgs, enums, exts);
            DefPoolInit(build_def_pool_init(arena, filename, descriptor, &deps, layout))
        }
    })
}

/// The process-wide pool.
///
/// upb's `DefPool` is not thread-safe -- loading a file inserts into its hash tables -- so
/// every access goes through the mutex rather than only the writes. The pool is never
/// dropped, which is what lets [`message_def`] hand out `MessageDef<'static>`.
struct GlobalPool(DefPool);

// SAFETY: `GlobalPool` is only reachable through the mutex in `POOL`, so every use of the
// non-thread-safe `DefPool` is serialized.
unsafe impl Send for GlobalPool {}

static POOL: OnceLock<Mutex<GlobalPool>> = OnceLock::new();

/// Returns the `MessageDef` of `M`, first loading the file that declares it, and everything
/// that file imports, into the global pool.
pub fn message_def<M: UpbWithReflection>() -> MessageDef<'static> {
    // Building the init builds the file's MiniTables, which never touches the pool, so do it
    // before taking the lock.
    let init = M::def_init();

    let mut pool = POOL
        .get_or_init(|| Mutex::new(GlobalPool(DefPool::new())))
        .lock()
        .expect("global DefPool mutex poisoned");

    // Loading a file that is already in the pool is a no-op, so this is cheap after the first
    // call for a file.
    //
    // SAFETY: `init` was built by `build_def_init` for the file that declares `M`, per the
    // safety requirements of `UpbWithReflection`, and it outlives the pool.
    let loaded = unsafe { pool.0.load_def_init(init.0.as_ptr()) };
    assert!(loaded, "a generated descriptor failed to load into the global DefPool");

    let def = pool
        .0
        .find_message_by_name(M::FULL_NAME)
        .expect("a generated message is missing from its own file's descriptor");

    // SAFETY: `def` was allocated by the global pool, which is never freed, so widening the
    // borrow of the guard to `'static` is sound. `MessageDef` is just a pointer.
    unsafe { MessageDef::from_raw(def.raw()) }
}
