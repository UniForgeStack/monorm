//! The C ABI of a native extension library, version 0.2.
//! The library exports `orm_ext_abi_version`, `orm_ext_run_pass` and `orm_ext_free` (0.1),
//! and in 0.2 the optional `orm_ext_init`, `orm_ext_drop_state` and `orm_ext_hooks`.
//! It links to neither Python nor Node. `extension!` writes the exports; the host loader calls them by these names and types.
//! A panic in a pass or a hook is caught, so it never crosses the ABI. The library must build with `panic = "unwind"`.
use crate::extension::ABI_VERSION;
use crate::ir::SchemaIr;
use std::any::Any;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

pub const SYMBOL_ABI_VERSION: &[u8] = b"orm_ext_abi_version\0";
pub const SYMBOL_RUN_PASS: &[u8] = b"orm_ext_run_pass\0";
pub const SYMBOL_FREE: &[u8] = b"orm_ext_free\0";
pub const SYMBOL_INIT: &[u8] = b"orm_ext_init\0";
pub const SYMBOL_DROP_STATE: &[u8] = b"orm_ext_drop_state\0";
pub const SYMBOL_HOOKS: &[u8] = b"orm_ext_hooks\0";

/// The pass or the hook ran; `out` holds the new IR JSON, or the value of the hook.
pub const STATUS_OK: i32 = 0;
/// The pass or the hook returned an error, or its new IR did not serialize; `out` holds the text.
pub const STATUS_ERROR: i32 = 1;
/// The pass or the hook panicked; `out` holds the panic text.
pub const STATUS_PANIC: i32 = 2;
/// The library has no pass of this id; `out` holds the text.
pub const STATUS_UNKNOWN_PASS: i32 = 3;
/// The id, the IR JSON, a value or the state did not parse or did not fit; `out` holds the text.
pub const STATUS_BAD_INPUT: i32 = 4;

/// A buffer that the library allocated. The host frees it with `orm_ext_free(ptr, len)`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Buffer { pub ptr: *mut u8, pub len: usize }

pub type PassFn = fn(&mut SchemaIr) -> Result<(), String>;
pub type AbiVersionFn = unsafe extern "C" fn() -> u32;
pub type RunPassFn = unsafe extern "C" fn(id: *const u8, id_len: usize, ir: *const u8, ir_len: usize, out: *mut Buffer) -> i32;
pub type FreeFn = unsafe extern "C" fn(ptr: *mut u8, len: usize);

pub type InitFn = unsafe extern "C" fn(data: *const u8, data_len: usize, state_out: *mut *mut c_void, error: *mut Buffer) -> i32;
pub type DropStateFn = unsafe extern "C" fn(state: *mut c_void);
pub type HooksFn = unsafe extern "C" fn() -> *const OrmHookTable;
/// One hook of any of the 7 points. `args` and `ctx` are borrowed for the call.
/// `out` is written on every status: the value on OK, the message as text on any other.
pub type HookFn = unsafe extern "C" fn(state: *const c_void, args: *const OrmArg, n: usize, ctx: *const OrmContext, out: *mut OrmValue) -> i32;

pub const fn version_bits() -> u32 { ABI_VERSION.to_bits() }

pub const TAG_NULL: u32 = 0;
pub const TAG_BOOL: u32 = 1;
pub const TAG_INT: u32 = 2;
pub const TAG_FLOAT: u32 = 3;
pub const TAG_DECIMAL: u32 = 4;
pub const TAG_TEXT: u32 = 5;
pub const TAG_BYTES: u32 = 6;
pub const TAG_DATETIME: u32 = 7;
pub const TAG_UUID: u32 = 8;
pub const TAG_JSON: u32 = 9;
pub const TAG_DATE: u32 = 10;

/// The typed value on the ABI. `tag` picks the field that holds it:
/// `int` for bool (0 or 1), int, datetime (microseconds since the Unix epoch, UTC) and date (days since 1970-01-01),
/// `float` for float, and `ptr`/`len` for decimal (canonical text), text (UTF-8), bytes, uuid (16 bytes) and json (JSON text).
/// An input value is borrowed for the call. An output value that has `ptr`/`len` is allocated by the library; the host copies it and calls `orm_ext_free(ptr, len)`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OrmValue { pub tag: u32, pub int: i64, pub float: f64, pub ptr: *const u8, pub len: usize }

impl OrmValue {
    pub const NULL: OrmValue = OrmValue { tag: TAG_NULL, int: 0, float: 0.0, ptr: std::ptr::null(), len: 0 };
}

/// A named argument. A request context is a list of these.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OrmArg { pub name_ptr: *const u8, pub name_len: usize, pub value: OrmValue }

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OrmContext { pub args: *const OrmArg, pub len: usize }

/// A named hook of a library.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OrmHookEntry { pub name_ptr: *const u8, pub name_len: usize, pub call: HookFn }

/// The static table that `orm_ext_hooks` returns.
#[repr(C)]
pub struct OrmHookTable { pub len: usize, pub entries: *const OrmHookEntry }

// SAFETY: a table and its entries are immutable statics of the library that point to other statics.
unsafe impl Sync for OrmHookTable {}
// SAFETY: see `OrmHookTable`.
unsafe impl Sync for OrmHookEntry {}

/// The value that a hook takes and returns, in Rust.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    /// Microseconds since the Unix epoch, UTC: an instant, not a local time.
    DateTime(i64),
    Uuid([u8; 16]),
    /// JSON text.
    Json(String),
    /// Days since 1970-01-01.
    Date(i32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg { pub name: String, pub value: Value }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context { pub values: Vec<Arg> }

impl Context {
    pub fn get(&self, name: &str) -> Option<&Value> { self.values.iter().find(|a| a.name == name).map(|a| &a.value) }
}

/// The hook of a Rust author. `S` is the state that `init` returns, or `()`.
pub type HookRust<S> = fn(&S, &[Arg], &Context) -> Result<Value, String>;
/// The `init` of a Rust author: the JSON of the `extension_data` entry in, the state out.
pub type InitRust<S> = fn(&serde_json::Value) -> Result<S, String>;

/// The `init` that the macro uses when the author gives none.
pub fn default_init(_: &serde_json::Value) -> Result<(), String> { Ok(()) }

fn leak_bytes(bytes: Vec<u8>) -> (*const u8, usize) {
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    (Box::into_raw(boxed) as *const u8, len)
}

impl Value {
    pub fn tag(&self) -> u32 {
        match self {
            Value::Null => TAG_NULL, Value::Bool(_) => TAG_BOOL, Value::Int(_) => TAG_INT, Value::Float(_) => TAG_FLOAT,
            Value::Decimal(_) => TAG_DECIMAL, Value::Text(_) => TAG_TEXT, Value::Bytes(_) => TAG_BYTES, Value::DateTime(_) => TAG_DATETIME,
            Value::Uuid(_) => TAG_UUID, Value::Json(_) => TAG_JSON, Value::Date(_) => TAG_DATE,
        }
    }

    fn raw(tag: u32, int: i64, float: f64, bytes: &[u8]) -> OrmValue { OrmValue { tag, int, float, ptr: bytes.as_ptr(), len: bytes.len() } }

    /// An `OrmValue` that points into `self`; it is valid while `self` lives and does not move.
    pub fn borrow(&self) -> OrmValue {
        let tag = self.tag();
        match self {
            Value::Null => OrmValue::NULL,
            Value::Bool(b) => Self::raw(tag, *b as i64, 0.0, &[]),
            Value::Int(i) | Value::DateTime(i) => Self::raw(tag, *i, 0.0, &[]),
            Value::Date(d) => Self::raw(tag, *d as i64, 0.0, &[]),
            Value::Float(f) => Self::raw(tag, 0, *f, &[]),
            Value::Decimal(s) | Value::Text(s) | Value::Json(s) => Self::raw(tag, 0, 0.0, s.as_bytes()),
            Value::Bytes(b) => Self::raw(tag, 0, 0.0, b),
            Value::Uuid(u) => Self::raw(tag, 0, 0.0, u),
        }
    }

    /// An `OrmValue` that owns its bytes. The host copies it and calls `orm_ext_free(ptr, len)`.
    pub fn leak(self) -> OrmValue {
        let tag = self.tag();
        let owned = |bytes: Vec<u8>| { let (ptr, len) = leak_bytes(bytes); OrmValue { tag, int: 0, float: 0.0, ptr, len } };
        match self {
            Value::Decimal(s) | Value::Text(s) | Value::Json(s) => owned(s.into_bytes()),
            Value::Bytes(b) => owned(b),
            Value::Uuid(u) => owned(u.to_vec()),
            other => other.borrow(),
        }
    }

    /// Copy a raw value into Rust.
    ///
    /// # Safety
    /// `raw.ptr` and `raw.len` describe a readable range when the tag uses them.
    pub unsafe fn from_raw(raw: &OrmValue) -> Result<Value, String> {
        // SAFETY: the caller guarantees the range.
        let bytes = || unsafe { bytes(raw.ptr, raw.len) };
        let text = || String::from_utf8(bytes().to_vec()).map_err(|_| format!("the value of tag {} is not UTF-8", raw.tag));
        Ok(match raw.tag {
            TAG_NULL => Value::Null,
            TAG_BOOL => match raw.int { 0 => Value::Bool(false), 1 => Value::Bool(true), n => return Err(format!("a bool value is {n}, not 0 or 1")) },
            TAG_INT => Value::Int(raw.int),
            TAG_FLOAT => Value::Float(raw.float),
            TAG_DECIMAL => Value::Decimal(text()?),
            TAG_TEXT => Value::Text(text()?),
            TAG_BYTES => Value::Bytes(bytes().to_vec()),
            TAG_DATETIME => Value::DateTime(raw.int),
            TAG_UUID => Value::Uuid(bytes().try_into().map_err(|_| format!("a uuid value has {} bytes, not 16", raw.len))?),
            TAG_JSON => Value::Json(text()?),
            TAG_DATE => Value::Date(i32::try_from(raw.int).map_err(|_| format!("a date value of {} days is out of range", raw.int))?),
            tag => return Err(format!("unknown value tag {tag}")),
        })
    }
}

impl Arg {
    /// An `OrmArg` that points into `self`.
    pub fn borrow(&self) -> OrmArg { OrmArg { name_ptr: self.name.as_ptr(), name_len: self.name.len(), value: self.value.borrow() } }

    /// # Safety
    /// The name range and the value ranges are readable.
    pub unsafe fn from_raw(raw: &OrmArg) -> Result<Arg, String> {
        // SAFETY: the caller guarantees the ranges.
        let name = String::from_utf8(unsafe { bytes(raw.name_ptr, raw.name_len) }.to_vec()).map_err(|_| "an argument name is not UTF-8".to_string())?;
        // SAFETY: the caller guarantees the ranges.
        Ok(Arg { name: name.clone(), value: unsafe { Value::from_raw(&raw.value) }.map_err(|e| format!("argument {name}: {e}"))? })
    }
}

/// # Safety
/// `ptr` and `len` describe a readable range of `T`, or `len` is 0.
unsafe fn items<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 || ptr.is_null() { return &[]; }
    // SAFETY: the caller guarantees the range.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn panic_text(payload: Box<dyn Any + Send>) -> String {
    payload.downcast_ref::<&str>().map(|s| (*s).to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "the call panicked".into())
}

/// The body of a hook export: converts the raw arguments, calls `hook` and writes `out`.
/// A null `state` is the state `()`; a state of another type than `S` is BAD_INPUT.
///
/// # Safety
/// `state` is null or comes from `init` of this library. `args`, `n`, `ctx` are readable, and `out` is writable.
pub unsafe fn call_hook<S: 'static>(hook: HookRust<S>, state: *const c_void, args: *const OrmArg, n: usize, ctx: *const OrmContext, out: *mut OrmValue) -> i32 {
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<Value, (i32, String)> {
        let state: &dyn Any = if state.is_null() { &() } else {
            // SAFETY: a non-null state is the `Box<Box<dyn Any>>` that `init` leaked, and the host keeps it alive during the call.
            unsafe { &**(state as *const Box<dyn Any>) }
        };
        let state = state.downcast_ref::<S>().ok_or_else(|| (STATUS_BAD_INPUT, "the state is not the type that this hook takes".to_string()))?;
        // SAFETY: the caller guarantees the ranges.
        let args = unsafe { items(args, n) }.iter().map(|a| unsafe { Arg::from_raw(a) }).collect::<Result<Vec<_>, _>>().map_err(|e| (STATUS_BAD_INPUT, e))?;
        let context = if ctx.is_null() { Context::default() } else {
            // SAFETY: the caller guarantees that a non-null `ctx` is readable and that its list is too.
            let raw = unsafe { &*ctx };
            Context { values: unsafe { items(raw.args, raw.len) }.iter().map(|a| unsafe { Arg::from_raw(a) }).collect::<Result<Vec<_>, _>>().map_err(|e| (STATUS_BAD_INPUT, e))? }
        };
        hook(state, &args, &context).map_err(|e| (STATUS_ERROR, e))
    }));
    let (status, value) = match outcome {
        Ok(Ok(value)) => (STATUS_OK, value),
        Ok(Err((status, text))) => (status, Value::Text(text)),
        Err(payload) => (STATUS_PANIC, Value::Text(panic_text(payload))),
    };
    // SAFETY: the caller guarantees that `out` is writable; the host frees the value with `free`.
    unsafe { *out = value.leak() };
    status
}

/// The body of `orm_ext_init`. An empty `data` is JSON null. On an error, `error` (if not null) holds the text.
///
/// # Safety
/// `data` is readable, and `state_out` is writable. `error` is null or writable.
pub unsafe fn init<S: 'static>(init: InitRust<S>, data: *const u8, data_len: usize, state_out: *mut *mut c_void, error: *mut Buffer) -> i32 {
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<S, (i32, String)> {
        // SAFETY: the caller guarantees the range.
        let data = unsafe { bytes(data, data_len) };
        let json: serde_json::Value = if data.is_empty() { serde_json::Value::Null } else {
            serde_json::from_slice(data).map_err(|e| (STATUS_BAD_INPUT, format!("the extension data does not parse: {e}")))?
        };
        init(&json).map_err(|e| (STATUS_ERROR, e))
    }));
    let (status, text) = match outcome {
        Ok(Ok(state)) => {
            let state: Box<dyn Any> = Box::new(state);
            // SAFETY: the caller guarantees that `state_out` is writable; `drop_state` frees the state once.
            unsafe { *state_out = Box::into_raw(Box::new(state)) as *mut c_void };
            (STATUS_OK, String::new())
        }
        Ok(Err((status, text))) => (status, text),
        Err(payload) => (STATUS_PANIC, panic_text(payload)),
    };
    if !error.is_null() {
        let (ptr, len) = leak_bytes(text.into_bytes());
        // SAFETY: the caller guarantees that a non-null `error` is writable.
        unsafe { *error = Buffer { ptr: ptr as *mut u8, len } };
    }
    status
}

/// The body of `orm_ext_drop_state`. A panic in the `Drop` of the state does not cross the ABI.
///
/// # Safety
/// `state` is null, or comes from `init` of this library, and nothing drops it twice.
pub unsafe fn drop_state(state: *mut c_void) {
    if state.is_null() { return; }
    // SAFETY: the caller guarantees that `state` is the `Box<Box<dyn Any>>` that `init` leaked, dropped once.
    let state = unsafe { Box::from_raw(state as *mut Box<dyn Any>) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(state)));
}

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
            (STATUS_PANIC, panic_text(payload).into_bytes())
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

/// Write the exports of ABI 0.2. Call it once, at the root of a `cdylib` crate:
///
/// ```ignore
/// orm_contracts::extension! {
///     init = my_init;                                  // optional: fn(&serde_json::Value) -> Result<S, String>
///     hooks { "my.hook" => my_hook, "my.other" => other }; // optional: fn(&S, &[Arg], &Context) -> Result<Value, String>
///     ("my.pass", my_pass), ("my.pass2", pass2)        // fn(&mut SchemaIr) -> Result<(), String>
/// }
/// ```
///
/// `abi = (major, minor);` first sets the reported ABI version (tests only). Without `init` the state is `()`.
/// The library must build with `panic = "unwind"`: the exports catch a panic and return `STATUS_PANIC`.
#[macro_export]
macro_rules! extension {
    (abi = ($major:expr, $minor:expr); $($rest:tt)*) => {
        $crate::extension!(@init ($crate::extension::ContractVersion::new($major, $minor).to_bits()); $($rest)*);
    };
    (@init ($bits:expr); init = $init:path; $($rest:tt)*) => {
        $crate::extension!(@hooks ($bits) ($init); $($rest)*);
    };
    (@init ($bits:expr); $($rest:tt)*) => {
        $crate::extension!(@hooks ($bits) ($crate::abi::default_init); $($rest)*);
    };
    (@hooks ($bits:expr) ($init:path); hooks { $($name:expr => $hook:path),* $(,)? } $(;)? $(($id:expr, $pass:expr)),* $(,)?) => {
        $crate::extension!(@exports ($bits) ($init) [$(($name, $hook))*]; $(($id, $pass)),*);
    };
    (@hooks ($bits:expr) ($init:path); $(($id:expr, $pass:expr)),* $(,)?) => {
        $crate::extension!(@exports ($bits) ($init) []; $(($id, $pass)),*);
    };
    (@exports ($bits:expr) ($init:path) [$(($name:expr, $hook:path))*]; $(($id:expr, $pass:expr)),*) => {
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
            // SAFETY: the loader passes a buffer that this library returned, once.
            unsafe { $crate::abi::free(ptr, len) }
        }
        /// # Safety
        /// Called by the ORM loader with a readable `data`, and a writable `state_out` and `error`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn orm_ext_init(data: *const u8, data_len: usize, state_out: *mut *mut ::std::ffi::c_void, error: *mut $crate::abi::Buffer) -> i32 {
            // SAFETY: the loader passes the arguments that `init` documents.
            unsafe { $crate::abi::init($init, data, data_len, state_out, error) }
        }
        /// # Safety
        /// Called by the ORM loader with a state of this library, once.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn orm_ext_drop_state(state: *mut ::std::ffi::c_void) {
            // SAFETY: the loader passes a state that `orm_ext_init` returned, once.
            unsafe { $crate::abi::drop_state(state) }
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn orm_ext_hooks() -> *const $crate::abi::OrmHookTable {
            const ENTRIES: &[$crate::abi::OrmHookEntry] = &[$($crate::abi::OrmHookEntry {
                name_ptr: $name.as_ptr(),
                name_len: $name.len(),
                call: {
                    unsafe extern "C" fn call(state: *const ::std::ffi::c_void, args: *const $crate::abi::OrmArg, n: usize, ctx: *const $crate::abi::OrmContext, out: *mut $crate::abi::OrmValue) -> i32 {
                        // SAFETY: the loader passes the arguments that `call_hook` documents.
                        unsafe { $crate::abi::call_hook($hook, state, args, n, ctx, out) }
                    }
                    call
                },
            }),*];
            static TABLE: $crate::abi::OrmHookTable = $crate::abi::OrmHookTable { len: ENTRIES.len(), entries: ENTRIES.as_ptr() };
            &TABLE
        }
    };
    ($($rest:tt)*) => {
        $crate::extension!(@init ($crate::abi::version_bits()); $($rest)*);
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

    fn every_value() -> Vec<Value> {
        vec![
            Value::Null, Value::Bool(true), Value::Bool(false), Value::Int(-7), Value::Float(2.5),
            Value::Decimal("12.3400".into()), Value::Text("héllo".into()), Value::Bytes(vec![0, 255, 1]),
            Value::DateTime(-1_500_000), Value::Uuid([7; 16]), Value::Json(r#"{"a":[1]}"#.into()), Value::Date(-3),
        ]
    }

    #[test]
    fn each_tag_converts_to_a_rust_value_and_back() {
        let tags: Vec<u32> = every_value().iter().map(Value::tag).collect();
        assert_eq!((0..=10).filter(|t| !tags.contains(t)).count(), 0, "every tag from 0 to 10 is covered");
        for value in every_value() {
            // SAFETY: `borrow` points into `value`, which lives through the call.
            assert_eq!(unsafe { Value::from_raw(&value.borrow()) }, Ok(value.clone()), "borrowed {value:?}");
            let leaked = value.clone().leak();
            // SAFETY: `leak` returns a buffer of `len` readable bytes (or no bytes).
            assert_eq!(unsafe { Value::from_raw(&leaked) }, Ok(value.clone()), "leaked {value:?}");
            // SAFETY: `leaked` is the buffer that `leak` returned, freed once.
            unsafe { free(leaked.ptr as *mut u8, leaked.len) };
        }
    }

    #[test]
    fn a_raw_value_that_does_not_fit_its_tag_is_an_error() {
        let bad = |tag, int, bytes: &[u8]| {
            // SAFETY: the range comes from a live slice.
            unsafe { Value::from_raw(&OrmValue { tag, int, float: 0.0, ptr: bytes.as_ptr(), len: bytes.len() }) }.unwrap_err()
        };
        assert!(bad(99, 0, &[]).contains("unknown value tag 99"));
        assert!(bad(TAG_BOOL, 2, &[]).contains("not 0 or 1"));
        assert!(bad(TAG_TEXT, 0, &[0xff]).contains("not UTF-8"));
        assert!(bad(TAG_UUID, 0, &[1, 2]).contains("2 bytes, not 16"));
        assert!(bad(TAG_DATE, i64::MAX, &[]).contains("out of range"));
    }

    struct Prefix(String);

    fn call<S: 'static>(hook: HookRust<S>, state: *const c_void, args: &[Arg], ctx: &Context) -> (i32, Value) {
        let raw: Vec<OrmArg> = args.iter().map(Arg::borrow).collect();
        let raw_ctx: Vec<OrmArg> = ctx.values.iter().map(Arg::borrow).collect();
        let ctx = OrmContext { args: raw_ctx.as_ptr(), len: raw_ctx.len() };
        let mut out = OrmValue::NULL;
        // SAFETY: the arrays and the context live through the call, and `out` is a writable local.
        let status = unsafe { call_hook(hook, state, raw.as_ptr(), raw.len(), &ctx, &mut out) };
        // SAFETY: `call_hook` always writes `out` with a value that `leak` made.
        let value = unsafe { Value::from_raw(&out) }.unwrap();
        // SAFETY: `out` is freed once.
        unsafe { free(out.ptr as *mut u8, out.len) };
        (status, value)
    }

    fn start(json: &str) -> *mut c_void {
        let mut state = std::ptr::null_mut();
        let mut error = Buffer { ptr: std::ptr::null_mut(), len: 0 };
        let init_fn: InitRust<Prefix> = |data| Ok(Prefix(data["prefix"].as_str().ok_or("no prefix")?.to_string()));
        // SAFETY: the range is a live string, and the outputs are writable locals.
        assert_eq!(unsafe { init(init_fn, json.as_ptr(), json.len(), &mut state, &mut error) }, STATUS_OK);
        assert!(!state.is_null() && error.len == 0);
        state
    }

    #[test]
    fn a_hook_gets_its_state_its_arguments_and_the_context_and_returns_a_value() {
        let state = start(r#"{"prefix":"x"}"#);
        let hook: HookRust<Prefix> = |s, args, ctx| {
            let Some(Arg { value: Value::Text(t), .. }) = args.first() else { return Err("one text argument".into()) };
            let tenant = match ctx.get("tenant") { Some(Value::Int(n)) => *n, _ => 0 };
            Ok(Value::Text(format!("{}{t}{tenant}", s.0)))
        };
        let args = [Arg { name: "value".into(), value: Value::Text("a".into()) }];
        let ctx = Context { values: vec![Arg { name: "tenant".into(), value: Value::Int(3) }] };
        assert_eq!(call(hook, state, &args, &ctx), (STATUS_OK, Value::Text("xa3".into())));
        assert_eq!(call(hook, state, &[], &ctx), (STATUS_ERROR, Value::Text("one text argument".into())));
        // SAFETY: `state` comes from `init` and is dropped once.
        unsafe { drop_state(state) };
    }

    #[test]
    fn a_panic_in_a_hook_is_a_status_and_the_state_survives() {
        let state = start(r#"{"prefix":"x"}"#);
        let panics: HookRust<Prefix> = |_, _, _| panic!("boom");
        assert_eq!(call(panics, state, &[], &Context::default()), (STATUS_PANIC, Value::Text("boom".into())));
        let fine: HookRust<Prefix> = |s, _, _| Ok(Value::Text(s.0.clone()));
        assert_eq!(call(fine, state, &[], &Context::default()), (STATUS_OK, Value::Text("x".into())));
        // SAFETY: `state` comes from `init` and is dropped once.
        unsafe { drop_state(state) };
    }

    #[test]
    fn a_state_of_another_type_and_a_bad_argument_are_bad_input() {
        let state = start(r#"{"prefix":"x"}"#);
        let other: HookRust<u8> = |_, _, _| Ok(Value::Null);
        let (status, text) = call(other, state, &[], &Context::default());
        assert_eq!(status, STATUS_BAD_INPUT);
        assert!(matches!(text, Value::Text(t) if t.contains("state")));
        let unit: HookRust<()> = |_, _, _| Ok(Value::Bool(true));
        assert_eq!(call(unit, std::ptr::null(), &[], &Context::default()), (STATUS_OK, Value::Bool(true)), "a null state is the unit state");
        let raw = OrmArg { name_ptr: b"a".as_ptr(), name_len: 1, value: OrmValue { tag: 99, ..OrmValue::NULL } };
        let mut out = OrmValue::NULL;
        // SAFETY: the one argument and `out` are live locals.
        let status = unsafe { call_hook(unit, std::ptr::null(), &raw, 1, std::ptr::null(), &mut out) };
        assert_eq!(status, STATUS_BAD_INPUT);
        // SAFETY: `call_hook` wrote a text value.
        let text = unsafe { Value::from_raw(&out) }.unwrap();
        assert!(matches!(text, Value::Text(t) if t.contains("argument a") && t.contains("tag 99")));
        // SAFETY: freed once.
        unsafe { free(out.ptr as *mut u8, out.len); drop_state(state) };
    }

    #[test]
    fn init_reports_an_error_a_parse_failure_and_an_empty_data_as_null() {
        let init_fn: InitRust<Prefix> = |data| Ok(Prefix(data["prefix"].as_str().ok_or("no prefix")?.to_string()));
        let run = |json: &str| {
            let mut state = std::ptr::null_mut();
            let mut error = Buffer { ptr: std::ptr::null_mut(), len: 0 };
            // SAFETY: the range is a live string, and the outputs are writable locals.
            let status = unsafe { init(init_fn, json.as_ptr(), json.len(), &mut state, &mut error) };
            // SAFETY: `init` wrote `error` with a buffer of `len` bytes.
            let text = String::from_utf8(unsafe { std::slice::from_raw_parts(error.ptr, error.len) }.to_vec()).unwrap();
            // SAFETY: freed once.
            unsafe { free(error.ptr, error.len) };
            (status, text, state)
        };
        let (status, text, state) = run("{}");
        assert_eq!((status, text.as_str(), state.is_null()), (STATUS_ERROR, "no prefix", true));
        let (status, text, _) = run("{");
        assert_eq!(status, STATUS_BAD_INPUT);
        assert!(text.contains("does not parse"), "{text}");
        assert_eq!(run("").0, STATUS_ERROR, "empty data is JSON null, which has no prefix");
    }

    #[test]
    fn dropping_a_state_runs_its_drop_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        struct Counted;
        impl Drop for Counted { fn drop(&mut self) { DROPS.fetch_add(1, Ordering::SeqCst); } }
        let init_fn: InitRust<Counted> = |_| Ok(Counted);
        let mut state = std::ptr::null_mut();
        // SAFETY: the outputs are writable locals; a null `error` is allowed.
        assert_eq!(unsafe { init(init_fn, std::ptr::null(), 0, &mut state, std::ptr::null_mut()) }, STATUS_OK);
        // SAFETY: `state` comes from `init`, dropped once; a null state is a no-op.
        unsafe { drop_state(state); drop_state(std::ptr::null_mut()) };
        assert_eq!(DROPS.load(Ordering::SeqCst), 1);
    }
}
