//! The C ABI of a native extension library, version 0.1.
//! The library exports three functions and links to neither Python nor Node.
//! `extension!` writes the exports; the host loader calls them by these names and types.
//! A panic in a pass is caught, so it never crosses the ABI. The library must build with `panic = "unwind"`.
use crate::extension::ABI_VERSION;
use crate::ir::SchemaIr;
use std::panic::{catch_unwind, AssertUnwindSafe};

pub const SYMBOL_ABI_VERSION: &[u8] = b"orm_ext_abi_version\0";
pub const SYMBOL_RUN_PASS: &[u8] = b"orm_ext_run_pass\0";
pub const SYMBOL_FREE: &[u8] = b"orm_ext_free\0";

/// The pass ran; `out` holds the new IR JSON.
pub const STATUS_OK: i32 = 0;
/// The pass returned an error, or its new IR did not serialize; `out` holds the text.
pub const STATUS_ERROR: i32 = 1;
/// The pass panicked; `out` holds the panic text.
pub const STATUS_PANIC: i32 = 2;
/// The library has no pass of this id; `out` holds the text.
pub const STATUS_UNKNOWN_PASS: i32 = 3;
/// The id or the IR JSON did not parse; `out` holds the text.
pub const STATUS_BAD_INPUT: i32 = 4;

/// A buffer that the library allocated. The host frees it with `orm_ext_free(ptr, len)`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Buffer { pub ptr: *mut u8, pub len: usize }

pub type PassFn = fn(&mut SchemaIr) -> Result<(), String>;
pub type AbiVersionFn = unsafe extern "C" fn() -> u32;
pub type RunPassFn = unsafe extern "C" fn(id: *const u8, id_len: usize, ir: *const u8, ir_len: usize, out: *mut Buffer) -> i32;
pub type FreeFn = unsafe extern "C" fn(ptr: *mut u8, len: usize);

pub const fn version_bits() -> u32 { ABI_VERSION.to_bits() }

/// # Safety
/// `ptr` and `len` describe a readable byte range, or `len` is 0.
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 || ptr.is_null() { return &[]; }
    // SAFETY: the caller guarantees that `ptr` and `len` describe a readable byte range.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// The body of `orm_ext_run_pass`.
///
/// # Safety
/// The byte ranges are readable and `out` is writable.
pub unsafe fn run_pass(passes: &[(&str, PassFn)], id: *const u8, id_len: usize, ir: *const u8, ir_len: usize, out: *mut Buffer) -> i32 {
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<Vec<u8>, (i32, String)> {
        // SAFETY: the caller of `run_pass` guarantees that both ranges are readable.
        let (id, ir) = unsafe { (bytes(id, id_len), bytes(ir, ir_len)) };
        let id = std::str::from_utf8(id).map_err(|_| (STATUS_BAD_INPUT, "the pass id is not UTF-8".to_string()))?;
        let pass = passes.iter().find(|(known, _)| *known == id).map(|(_, pass)| *pass)
            .ok_or_else(|| (STATUS_UNKNOWN_PASS, format!("this library has no pass {id}")))?;
        let mut ir: SchemaIr = serde_json::from_slice(ir).map_err(|e| (STATUS_BAD_INPUT, format!("the IR JSON does not parse: {e}")))?;
        pass(&mut ir).map_err(|e| (STATUS_ERROR, e))?;
        serde_json::to_vec(&ir).map_err(|e| (STATUS_ERROR, format!("the IR does not serialize: {e}")))
    }));
    let (status, text) = match outcome {
        Ok(Ok(json)) => (STATUS_OK, json),
        Ok(Err((status, text))) => (status, text.into_bytes()),
        Err(payload) => {
            let text = payload.downcast_ref::<&str>().map(|s| (*s).to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "the pass panicked".into());
            (STATUS_PANIC, text.into_bytes())
        }
    };
    let boxed = text.into_boxed_slice();
    let len = boxed.len();
    // SAFETY: the caller of `run_pass` guarantees that `out` is writable; the host frees the buffer with `free`.
    unsafe { *out = Buffer { ptr: Box::into_raw(boxed) as *mut u8, len } };
    status
}

/// The body of `orm_ext_free`.
///
/// # Safety
/// `ptr` and `len` come from a `Buffer` of this library, and nothing frees them twice.
pub unsafe fn free(ptr: *mut u8, len: usize) {
    if ptr.is_null() { return; }
    // SAFETY: the caller guarantees that `ptr` and `len` are the `Box<[u8]>` that `run_pass` leaked, and that nothing frees it twice.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
}

/// Write the exports of ABI 0.1 for the passes of a library, as `(id, fn(&mut SchemaIr) -> Result<(), String>)`.
/// Call it once, at the root of a `cdylib` crate. The second form sets the reported ABI version (tests only).
#[macro_export]
macro_rules! extension {
    (abi = ($major:expr, $minor:expr); $(($id:expr, $pass:expr)),* $(,)?) => {
        $crate::extension!(@exports ($crate::extension::ContractVersion::new($major, $minor).to_bits()); $(($id, $pass)),*);
    };
    ($(($id:expr, $pass:expr)),* $(,)?) => {
        $crate::extension!(@exports ($crate::abi::version_bits()); $(($id, $pass)),*);
    };
    (@exports ($bits:expr); $(($id:expr, $pass:expr)),*) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn orm_ext_abi_version() -> u32 { $bits }
        /// # Safety
        /// Called by the ORM loader with readable ranges and a writable `out`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn orm_ext_run_pass(id: *const u8, id_len: usize, ir: *const u8, ir_len: usize, out: *mut $crate::abi::Buffer) -> i32 {
            let passes: &[(&str, $crate::abi::PassFn)] = &[$(($id, $pass as $crate::abi::PassFn)),*];
            // SAFETY: the loader passes the ranges and the `out` that `run_pass` documents, and this function hands them on unchanged.
            unsafe { $crate::abi::run_pass(passes, id, id_len, ir, ir_len, out) }
        }
        /// # Safety
        /// Called by the ORM loader with a buffer of this library.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn orm_ext_free(ptr: *mut u8, len: usize) {
            // SAFETY: the loader passes a buffer that `orm_ext_run_pass` of this library returned, once.
            unsafe { $crate::abi::free(ptr, len) }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, ir: &str) -> (i32, String) {
        fn fine(ir: &mut SchemaIr) -> Result<(), String> { ir.behavior.lowered_models.push("Touched".into()); Ok(()) }
        fn fails(_: &mut SchemaIr) -> Result<(), String> { Err("no good".into()) }
        fn panics(_: &mut SchemaIr) -> Result<(), String> { panic!("boom") }
        let passes: &[(&str, PassFn)] = &[("t.fine", fine), ("t.fails", fails), ("t.panics", panics)];
        let mut out = Buffer { ptr: std::ptr::null_mut(), len: 0 };
        // SAFETY: the ranges come from live strings and `out` is a writable local.
        let status = unsafe { run_pass(passes, id.as_ptr(), id.len(), ir.as_ptr(), ir.len(), &mut out) };
        // SAFETY: `run_pass` always writes `out` with a buffer of `len` readable bytes.
        let text = String::from_utf8(unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec()).unwrap();
        // SAFETY: `out` is the buffer that `run_pass` returned, freed here once.
        unsafe { free(out.ptr, out.len) };
        (status, text)
    }

    #[test]
    fn a_pass_returns_the_new_ir_json() {
        let ir = serde_json::to_string(&SchemaIr::default()).unwrap();
        let (status, text) = run("t.fine", &ir);
        assert_eq!(status, STATUS_OK);
        assert_eq!(serde_json::from_str::<SchemaIr>(&text).unwrap().behavior.lowered_models, ["Touched"]);
    }

    #[test]
    fn an_error_a_panic_and_bad_input_come_back_as_a_status_and_text() {
        let ir = serde_json::to_string(&SchemaIr::default()).unwrap();
        assert_eq!(run("t.fails", &ir), (STATUS_ERROR, "no good".to_string()));
        assert_eq!(run("t.panics", &ir), (STATUS_PANIC, "boom".to_string()));
        assert_eq!(run("t.missing", &ir).0, STATUS_UNKNOWN_PASS);
        let (status, text) = run("t.fine", "{");
        assert_eq!(status, STATUS_BAD_INPUT);
        assert!(text.contains("IR JSON"), "{text}");
    }
}
