//! Flake evaluation-cache walk.
//!
//! A cursor-driven, eval-cache-warm walk over a locked flake's output
//! attributes. When the flake source is immutable the walk is served from the
//! on-disk `AttrDb`; otherwise it falls back to live evaluation.
//!
//! Types:
//!
//! - [`EvalCache`]: an open eval-cache for a [`LockedFlake`]; produced by
//!   [`EvalCache::open`]. Call [`EvalCache::root`] for the root cursor.
//! - [`AttrCursor`]: a cursor into the output attribute tree; descend with
//!   [`AttrCursor::maybe_get_attr`] and read leaves with
//!   [`AttrCursor::string`], [`AttrCursor::bool`], [`AttrCursor::drv_path`],
//!   etc.

use std::{ffi::CString, ptr::NonNull, sync::Arc};

use crate::{
  Context, Error, EvalState, Result, check_err, flake::LockedFlake,
  string_from_callback, sys,
};

/// Collector for the `(name, user_data)` list callbacks; pushes each string
/// into a `Vec<String>` owned by `user_data`.
unsafe extern "C" fn collect_into_vec(
  s: *const std::os::raw::c_char,
  user_data: *mut std::os::raw::c_void,
) {
  if s.is_null() {
    return;
  }

  let out = unsafe { &mut *(user_data as *mut Vec<String>) };
  if let Ok(text) = unsafe { std::ffi::CStr::from_ptr(s) }.to_str() {
    out.push(text.to_owned());
  }
}

/// A Nix flake's eval-cache.
///
/// Warm (reads the on-disk `AttrDb`) only when the [`EvalState`] was built with
/// pure-eval and use-eval-cache and the flake source is immutable; otherwise it
/// evaluates live. Keep the [`EvalState`] it was opened against alive while
/// walking.
pub struct EvalCache {
  inner:    NonNull<sys::nix_eval_cache>,
  _context: Arc<Context>,
}

impl EvalCache {
  /// Open the eval-cache for a locked flake.
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call returns a null pointer.
  pub fn open(
    context: &Arc<Context>,
    eval_state: &EvalState,
    locked: &LockedFlake,
  ) -> Result<Self> {
    // SAFETY: all pointers are valid for the duration of the call
    let ptr = unsafe {
      sys::nix_eval_cache_open(
        context.as_ptr(),
        eval_state.as_ptr(),
        locked.as_ptr(),
      )
    };

    let inner = NonNull::new(ptr).ok_or(Error::NullPointer)?;

    Ok(EvalCache {
      inner,
      _context: Arc::clone(context),
    })
  }

  /// The root cursor, positioned at the flake's `outputs`.
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call returns a null pointer.
  pub fn root(&self) -> Result<AttrCursor> {
    // SAFETY: context and cache are valid
    let ptr = unsafe {
      sys::nix_eval_cache_get_root(self._context.as_ptr(), self.inner.as_ptr())
    };

    let inner = NonNull::new(ptr).ok_or(Error::NullPointer)?;

    Ok(AttrCursor {
      inner,
      _context: Arc::clone(&self._context),
    })
  }

  /// Commit pending writes into the cache's SQLite WAL **without** checkpointing.
  /// Safe to call concurrently from several evaluators sharing one cache; a
  /// checkpoint here would deadlock on the WAL read-slot locks. Use
  /// [`Self::checkpoint`] to fold the WAL into the main `.sqlite` file.
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call fails.
  pub fn commit(&self) -> Result<()> {
    // SAFETY: context and cache are valid for the duration of the call
    let err = unsafe {
      sys::nix_eval_cache_commit(self._context.as_ptr(), self.inner.as_ptr())
    };

    check_err(unsafe { self._context.as_ptr() }, err)
  }

  /// Fold the WAL into the main `.sqlite` file (PASSIVE checkpoint), so a reader
  /// of the file alone sees the committed writes. Never blocks: it does not take
  /// the exclusive WAL read-slot lock, so it is safe to call while other
  /// connections hold read locks (e.g. a concurrent evaluator of the same flake).
  /// A lone caller folds the whole WAL; under concurrency it folds what it can.
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call fails.
  pub fn checkpoint(&self) -> Result<()> {
    // SAFETY: context and cache are valid for the duration of the call
    let err = unsafe {
      sys::nix_eval_cache_checkpoint(self._context.as_ptr(), self.inner.as_ptr())
    };

    check_err(unsafe { self._context.as_ptr() }, err)
  }
}

impl Drop for EvalCache {
  fn drop(&mut self) {
    // SAFETY: we own the cache and it is valid until drop
    unsafe {
      sys::nix_eval_cache_free(self.inner.as_ptr());
    }
  }
}

// SAFETY: EvalCache can be shared between threads
unsafe impl Send for EvalCache {}
unsafe impl Sync for EvalCache {}

/// A cursor into a flake's output attribute tree (eval-cache backed).
pub struct AttrCursor {
  inner:    NonNull<sys::nix_attr_cursor>,
  _context: Arc<Context>,
}

impl AttrCursor {
  /// Descend into a child attribute, or `None` if it does not exist.
  ///
  /// # Errors
  ///
  /// Returns an error only on an actual evaluation or type error; a missing
  /// attribute yields `Ok(None)`.
  pub fn maybe_get_attr(&self, name: &str) -> Result<Option<AttrCursor>> {
    let name_c = CString::new(name)?;

    // SAFETY: context, cursor, and name are valid
    let ptr = unsafe {
      sys::nix_attr_cursor_maybe_get_attr(
        self._context.as_ptr(),
        self.inner.as_ptr(),
        name_c.as_ptr(),
      )
    };

    if let Some(inner) = NonNull::new(ptr) {
      return Ok(Some(AttrCursor {
        inner,
        _context: Arc::clone(&self._context),
      }));
    }

    // NULL is ambiguous: a missing attr (NIX_OK) or an exception (err set).
    // Read the context's last error code to distinguish the two.
    let code =
      unsafe { sys::nix_err_code(self._context.as_ptr() as *const _) };
    if code == sys::nix_err_NIX_OK {
      Ok(None)
    } else {
      check_err(unsafe { self._context.as_ptr() }, code)?;
      Ok(None)
    }
  }

  /// Enumerate child attribute names. Needs the [`EvalState`] to resolve
  /// symbols to names.
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call fails.
  pub fn attrs(&self, eval_state: &EvalState) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();

    // SAFETY: all pointers are valid; the callback collects names into `out`
    let err = unsafe {
      sys::nix_attr_cursor_get_attrs(
        self._context.as_ptr(),
        eval_state.as_ptr(),
        self.inner.as_ptr(),
        Some(collect_into_vec),
        &mut out as *mut Vec<String> as *mut std::os::raw::c_void,
      )
    };
    check_err(unsafe { self._context.as_ptr() }, err)?;

    Ok(out)
  }

  /// Whether this attr is a derivation (`.type == "derivation"`).
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call fails.
  pub fn is_derivation(&self) -> Result<bool> {
    let mut out = false;

    // SAFETY: context and cursor are valid; `out` is a writable bool
    let err = unsafe {
      sys::nix_attr_cursor_is_derivation(
        self._context.as_ptr(),
        self.inner.as_ptr(),
        &mut out,
      )
    };
    check_err(unsafe { self._context.as_ptr() }, err)?;

    Ok(out)
  }

  /// The derivation's `drvPath`, forcing the `.drv` into the store. Needs the
  /// [`EvalState`] whose store prints the path.
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call fails or yields no string.
  pub fn drv_path(&self, eval_state: &EvalState) -> Result<String> {
    let mut err = sys::nix_err_NIX_OK;

    // SAFETY: all pointers are valid; the callback collects the path string
    let result = unsafe {
      string_from_callback(|cb, ud| {
        err = sys::nix_attr_cursor_get_drv_path(
          self._context.as_ptr(),
          eval_state.as_ptr(),
          self.inner.as_ptr(),
          cb,
          ud,
        );
      })
    };
    check_err(unsafe { self._context.as_ptr() }, err)?;

    result.ok_or(Error::NullPointer)
  }

  /// The attr as a string (e.g. the `system` field).
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call fails or the attr is not a string.
  pub fn string(&self) -> Result<String> {
    let mut err = sys::nix_err_NIX_OK;

    // SAFETY: context and cursor are valid; the callback collects the string
    let result = unsafe {
      string_from_callback(|cb, ud| {
        err = sys::nix_attr_cursor_get_string(
          self._context.as_ptr(),
          self.inner.as_ptr(),
          cb,
          ud,
        );
      })
    };
    check_err(unsafe { self._context.as_ptr() }, err)?;

    result.ok_or(Error::NullPointer)
  }

  /// The attr as a bool (e.g. `recurseForDerivations`).
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call fails or the attr is not a bool.
  pub fn bool(&self) -> Result<bool> {
    let mut out = false;

    // SAFETY: context and cursor are valid; `out` is a writable bool
    let err = unsafe {
      sys::nix_attr_cursor_get_bool(
        self._context.as_ptr(),
        self.inner.as_ptr(),
        &mut out,
      )
    };
    check_err(unsafe { self._context.as_ptr() }, err)?;

    Ok(out)
  }

  /// The attr as a list of strings (e.g. `meta.requiredSystemFeatures`).
  ///
  /// # Errors
  ///
  /// Returns an error if the C API call fails or any element is not a string.
  pub fn list_of_strings(&self) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();

    // SAFETY: context and cursor are valid; the callback collects into `out`
    let err = unsafe {
      sys::nix_attr_cursor_get_list_of_strings(
        self._context.as_ptr(),
        self.inner.as_ptr(),
        Some(collect_into_vec),
        &mut out as *mut Vec<String> as *mut std::os::raw::c_void,
      )
    };
    check_err(unsafe { self._context.as_ptr() }, err)?;

    Ok(out)
  }
}

impl Drop for AttrCursor {
  fn drop(&mut self) {
    // SAFETY: we own the cursor and it is valid until drop
    unsafe {
      sys::nix_attr_cursor_free(self.inner.as_ptr());
    }
  }
}

// SAFETY: AttrCursor can be shared between threads
unsafe impl Send for AttrCursor {}
unsafe impl Sync for AttrCursor {}

#[cfg(test)]
mod tests {
  use std::{io::Write, sync::Arc};

  use serial_test::serial;

  use super::*;
  use crate::{
    Context, EvalStateBuilder, Store,
    flake::{
      FetchersSettings, FlakeReference, FlakeReferenceParseFlags,
      FlakeSettings, LockFlags,
    },
  };

  const FLAKE: &str = r#"{
    outputs = { ... }: {
      packages.x86_64-linux.hello = derivation {
        name = "hello";
        system = "x86_64-linux";
        builder = "/bin/sh";
      };
    };
  }"#;

  #[test]
  #[serial]
  fn test_eval_cache_walk() {
    let ctx = Arc::new(Context::new().expect("ctx"));
    let store = Arc::new(Store::open(&ctx, None).expect("store"));
    let fetch = FetchersSettings::new(&ctx).expect("fetch settings");
    let flake_settings =
      Arc::new(FlakeSettings::new(&ctx).expect("flake settings"));
    let state = EvalStateBuilder::new(&store)
      .expect("builder")
      .with_flake_settings(&flake_settings)
      .expect("with flake settings")
      .set_setting("eval-cache", "true")
      .expect("eval-cache")
      .set_setting("pure-eval", "true")
      .expect("pure-eval")
      .build()
      .expect("state");

    let dir = std::env::temp_dir()
      .join(format!("nix-bindings-eval-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut f =
      std::fs::File::create(dir.join("flake.nix")).expect("flake.nix");
    f.write_all(FLAKE.as_bytes()).expect("write");

    let parse_flags = FlakeReferenceParseFlags::new(&ctx, &flake_settings)
      .expect("parse flags")
      .set_base_directory(dir.to_str().unwrap())
      .expect("base dir");
    let (flake_ref, _frag) =
      FlakeReference::parse(&ctx, &fetch, &flake_settings, &parse_flags, ".")
        .expect("parse ref");
    let lock_flags =
      LockFlags::new(&ctx, &flake_settings).expect("lock flags");
    let locked = LockedFlake::lock(
      &ctx, &fetch, &flake_settings, &state, &lock_flags, &flake_ref,
    )
    .expect("lock");

    let cache = EvalCache::open(&ctx, &state, &locked).expect("open cache");
    let root = cache.root().expect("root");

    let packages =
      root.maybe_get_attr("packages").expect("packages").expect("present");
    let system = packages
      .maybe_get_attr("x86_64-linux")
      .expect("system")
      .expect("present");

    let names = system.attrs(&state).expect("attrs");
    assert!(!names.is_empty());
    assert!(names.iter().any(|n| n == "hello"));

    let hello =
      system.maybe_get_attr("hello").expect("hello").expect("present");
    assert!(hello.is_derivation().expect("is_derivation"));
    let drv = hello.drv_path(&state).expect("drv_path");
    assert!(drv.ends_with(".drv"));

    assert!(
      system.maybe_get_attr("definitely_absent").expect("absent").is_none()
    );
  }
}
