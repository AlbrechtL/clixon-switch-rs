//! Safe interface for clixon backend plugins.
//!
//! A plugin implements [`BackendPlugin`] and exports itself with
//! [`export_backend_plugin!`], which defines the `clixon_plugin_init` symbol
//! that clixon_backend looks up after dlopen().
//!
//! Every callback runs inside `catch_unwind`. A returned [`Error`] or a panic
//! is reported through `clixon_err` and fails the callback with -1; in a
//! transaction clixon turns that into an rpc-error for the client, and the
//! backend keeps running.
//!
//! clixon_backend is single-threaded, so callbacks never run concurrently.

use std::ffi::{c_char, c_int, CStr, CString};
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::sync::{Mutex, PoisonError};

pub use clixon_sys as sys;

/// The clixon handle passed to every callback.
#[derive(Clone, Copy)]
pub struct Handle(sys::clixon_handle);

// clixon_backend is single-threaded and the handle lives for the whole
// process, so a plugin may keep it in its state, which sits in a static.
unsafe impl Send for Handle {}

#[derive(Debug, Clone, Copy)]
pub enum Level {
    Error,
    Warning,
    Notice,
    Info,
    Debug,
}

impl Handle {
    /// Logs through clixon, i.e. to syslog, stderr or a file as selected
    /// with clixon_backend -l.
    pub fn log(self, level: Level, message: &str) {
        let level = match level {
            Level::Error => sys::LOG_ERR,
            Level::Warning => sys::LOG_WARNING,
            Level::Notice => sys::LOG_NOTICE,
            Level::Info => sys::LOG_INFO,
            Level::Debug => sys::LOG_DEBUG,
        };
        let message = c_string(message);
        // Never pass the message itself as the format.
        unsafe {
            sys::clixon_log_fn(
                self.0,
                1,
                level,
                ptr::null_mut(),
                c"%s".as_ptr(),
                message.as_ptr(),
            );
        }
    }

    /// Sets the clixon error that the failing callback reports.
    fn error(self, plugin: &CStr, message: &str) {
        let message = c_string(message);
        unsafe {
            sys::clixon_err_fn(
                self.0,
                plugin.as_ptr(),
                0,
                sys::OE_PLUGIN,
                0,
                ptr::null_mut(),
                c"%s".as_ptr(),
                message.as_ptr(),
            );
        }
    }
}

/// Error returned by plugin callbacks. Converts from any
/// [`std::error::Error`], so `?` works on them.
pub struct Error(String);

impl Error {
    pub fn msg(message: impl fmt::Display) -> Self {
        Error(message.to_string())
    }
}

impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Self {
        Error(e.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A commit in progress: the datastore tree before (src) and after (target).
pub struct Transaction(sys::transaction_data);

impl Transaction {
    /// The current configuration, as RFC 7951 JSON. Empty if there is none.
    pub fn src_json(&self) -> Result<String> {
        tree_json(unsafe { sys::transaction_src(self.0) })
    }

    /// The configuration being committed, as RFC 7951 JSON. Empty if the
    /// commit deletes everything.
    pub fn target_json(&self) -> Result<String> {
        tree_json(unsafe { sys::transaction_target(self.0) })
    }
}

fn tree_json(tree: *mut sys::cxobj) -> Result<String> {
    if tree.is_null() {
        return Ok(String::new());
    }
    let buf = CBuf::new()?;
    // skiptop: the top node is the datastore's <config>, not data. The
    // vec variant prints its children as members of one JSON object;
    // clixon_json2cbuf would print one object per child, comma-separated.
    let mut vec = [tree];
    if unsafe { sys::xml2json_cbuf_vec(buf.0, vec.as_mut_ptr(), vec.len(), 0, 1) } < 0 {
        return Err(Error::msg("xml2json_cbuf_vec failed"));
    }
    Ok(buf.to_string_lossy())
}

struct CBuf(*mut sys::cbuf);

impl CBuf {
    fn new() -> Result<Self> {
        let cb = unsafe { sys::cbuf_new() };
        if cb.is_null() {
            return Err(Error::msg("cbuf_new failed"));
        }
        Ok(CBuf(cb))
    }

    fn to_string_lossy(&self) -> String {
        unsafe { CStr::from_ptr(sys::cbuf_get(self.0)) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for CBuf {
    fn drop(&mut self) {
        unsafe { sys::cbuf_free(self.0) }
    }
}

/// The tree that state data is added to in [`BackendPlugin::statedata`].
pub struct StateTree(*mut sys::cxobj);

impl StateTree {
    /// Parses `xml` (with namespaces, e.g.
    /// `<interfaces xmlns="http://openconfig.net/yang/interfaces">...`) and
    /// merges it into the tree. clixon binds it to YANG afterwards.
    pub fn add_xml(&mut self, xml: &str) -> Result<()> {
        let xml = CString::new(xml).map_err(Error::msg)?;
        let rc = unsafe {
            sys::clixon_xml_parse_string1(
                ptr::null_mut(),
                xml.as_ptr(),
                sys::YB_NONE,
                ptr::null_mut(),
                &mut self.0,
                ptr::null_mut(),
            )
        };
        if rc < 0 {
            return Err(Error::msg("cannot parse state data XML"));
        }
        Ok(())
    }
}

/// Callbacks of a backend plugin. All have a no-op default.
pub trait BackendPlugin: Send {
    /// Called once, after all plugins are loaded and before the startup
    /// configuration is committed.
    fn start(&mut self, _h: Handle) -> Result<()> {
        Ok(())
    }

    /// Called when clixon_backend exits.
    fn exit(&mut self, _h: Handle) -> Result<()> {
        Ok(())
    }

    /// Checks the target configuration. An error rejects the commit before
    /// anything is applied.
    fn trans_validate(&mut self, _h: Handle, _tx: &Transaction) -> Result<()> {
        Ok(())
    }

    /// Applies the target configuration. An error aborts the commit, and
    /// clixon calls [`BackendPlugin::trans_revert`] on the plugins that had
    /// already committed.
    fn trans_commit(&mut self, _h: Handle, _tx: &Transaction) -> Result<()> {
        Ok(())
    }

    /// Restores the src configuration after a failed commit.
    fn trans_revert(&mut self, _h: Handle, _tx: &Transaction) -> Result<()> {
        Ok(())
    }

    /// Adds operational state for a get request. `xpath` narrows what the
    /// client asked for; adding more is allowed, clixon filters.
    fn statedata(
        &mut self,
        _h: Handle,
        _xpath: Option<&str>,
        _state: &mut StateTree,
    ) -> Result<()> {
        Ok(())
    }
}

/// Defines `clixon_plugin_init` for a [`BackendPlugin`].
///
/// `$name` is the plugin name in clixon's logs, `$init` a
/// `fn(Handle) -> Result<P>` that creates the plugin.
///
/// ```ignore
/// clixon_plugin::export_backend_plugin!("clixon-switch", SwitchPlugin::new);
/// ```
#[macro_export]
macro_rules! export_backend_plugin {
    ($name:literal, $init:path) => {
        #[no_mangle]
        pub unsafe extern "C" fn clixon_plugin_init(
            h: $crate::sys::clixon_handle,
        ) -> *mut $crate::sys::clixon_plugin_api {
            $crate::__private::init(h, $name, |h| {
                $init(h).map(|p| {
                    ::std::boxed::Box::new(p) as ::std::boxed::Box<dyn $crate::BackendPlugin>
                })
            })
        }
    };
}

#[doc(hidden)]
pub mod __private {
    use super::*;

    static PLUGIN: Mutex<Option<Registered>> = Mutex::new(None);

    struct Registered {
        name: CString,
        plugin: Box<dyn BackendPlugin>,
    }

    pub fn init(
        h: sys::clixon_handle,
        name: &str,
        create: impl FnOnce(Handle) -> Result<Box<dyn BackendPlugin>>,
    ) -> *mut sys::clixon_plugin_api {
        let handle = Handle(h);
        let name = c_string(name);

        let created = panic::catch_unwind(AssertUnwindSafe(|| create(handle)));
        let plugin = match created {
            Ok(Ok(plugin)) => plugin,
            Ok(Err(e)) => {
                handle.error(&name, &format!("init: {e}"));
                return ptr::null_mut();
            }
            Err(payload) => {
                handle.error(&name, &format!("init: panic: {}", panic_message(&*payload)));
                return ptr::null_mut();
            }
        };

        let mut api = Box::new(sys::clixon_plugin_api {
            ca_name: [0; sys::MAXPATHLEN],
            ca_init: Some(clixon_plugin_init_unused),
            ca_start: Some(start),
            ca_exit: Some(exit),
            ca_extension: None,
            ca_yang_mount: None,
            ca_yang_patch: None,
            ca_errmsg: None,
            ca_version: None,
            ca_userdef: None,
            u: sys::backend_api {
                cb_pre_daemon: None,
                cb_daemon: None,
                cb_reset: None,
                cb_statedata: Some(statedata),
                cb_system_only: None,
                cb_lockdb: None,
                cb_trans_begin: None,
                cb_trans_validate: Some(trans_validate),
                cb_trans_complete: None,
                cb_trans_commit: Some(trans_commit),
                cb_trans_commit_done: None,
                cb_trans_commit_failed: None,
                cb_trans_revert: Some(trans_revert),
                cb_trans_end: None,
                cb_trans_abort: None,
                cb_datastore_upgrade: None,
            },
        });
        let bytes = name.as_bytes();
        let len = bytes.len().min(sys::MAXPATHLEN - 1);
        for (dst, src) in api.ca_name.iter_mut().zip(&bytes[..len]) {
            *dst = *src as c_char;
        }

        *PLUGIN.lock().unwrap_or_else(PoisonError::into_inner) = Some(Registered { name, plugin });
        // clixon keeps using the struct for the lifetime of the process.
        Box::into_raw(api)
    }

    /// ca_init is informational: clixon has already called the exported
    /// symbol by the time it reads the struct.
    unsafe extern "C" fn clixon_plugin_init_unused(
        _h: sys::clixon_handle,
    ) -> *mut sys::clixon_plugin_api {
        ptr::null_mut()
    }

    /// Runs `f` on the registered plugin, turning errors and panics into
    /// a clixon error and -1.
    fn call(
        h: sys::clixon_handle,
        what: &str,
        f: impl FnOnce(&mut dyn BackendPlugin, Handle) -> Result<()>,
    ) -> c_int {
        let handle = Handle(h);
        let mut guard = PLUGIN.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(registered) = guard.as_mut() else {
            return -1;
        };

        let outcome =
            panic::catch_unwind(AssertUnwindSafe(|| f(registered.plugin.as_mut(), handle)));
        let message = match outcome {
            Ok(Ok(())) => return 0,
            Ok(Err(e)) => format!("{what}: {e}"),
            Err(payload) => format!("{what}: panic: {}", panic_message(&*payload)),
        };
        handle.error(&registered.name, &message);
        -1
    }

    unsafe extern "C" fn start(h: sys::clixon_handle) -> c_int {
        call(h, "start", |p, h| p.start(h))
    }

    unsafe extern "C" fn exit(h: sys::clixon_handle) -> c_int {
        call(h, "exit", |p, h| p.exit(h))
    }

    unsafe extern "C" fn trans_validate(h: sys::clixon_handle, td: sys::transaction_data) -> c_int {
        call(h, "validate", |p, h| p.trans_validate(h, &Transaction(td)))
    }

    unsafe extern "C" fn trans_commit(h: sys::clixon_handle, td: sys::transaction_data) -> c_int {
        call(h, "commit", |p, h| p.trans_commit(h, &Transaction(td)))
    }

    unsafe extern "C" fn trans_revert(h: sys::clixon_handle, td: sys::transaction_data) -> c_int {
        call(h, "revert", |p, h| p.trans_revert(h, &Transaction(td)))
    }

    unsafe extern "C" fn statedata(
        h: sys::clixon_handle,
        _nsc: *mut sys::cvec,
        xpath: *mut c_char,
        xconfig: *mut sys::cxobj,
    ) -> c_int {
        let xpath = (!xpath.is_null()).then(|| unsafe { CStr::from_ptr(xpath) }.to_string_lossy());
        call(h, "statedata", |p, h| {
            p.statedata(h, xpath.as_deref(), &mut StateTree(xconfig))
        })
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

/// CString of `s`; interior NULs (which C cannot carry) become spaces.
fn c_string(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).expect("NULs were replaced")
}
