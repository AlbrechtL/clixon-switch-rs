//! The admin account's password and the factory reset.
//!
//! The password lives in /etc/shadow. Setting it and the factory reset are
//! done by scripts of the firmware (see [`AccountConfig`]), so that root on
//! the serial console runs the same code as the RPCs. This module decides
//! whether a request may go ahead and checks passwords.

use std::ffi::{c_char, c_void, CStr, CString};
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Shortest password accepted.
pub const MIN_PASSWORD_LEN: usize = 8;
/// Longest password accepted: more is a mistake, not a password.
pub const MAX_PASSWORD_LEN: usize = 128;

/// Where the firmware's pieces are.
#[derive(Debug, Clone)]
pub struct AccountConfig {
    /// The account the RPCs change the password of.
    pub user: String,
    /// Exists while no password was set: first-login setup.
    pub setup_flag: PathBuf,
    /// `set-password USER`, reads the new password on stdin.
    pub set_password: PathBuf,
    /// `factory-reset --later`: wipes the data partition on the next boot
    /// and reboots in the background.
    pub factory_reset: PathBuf,
    pub shadow: PathBuf,
}

impl Default for AccountConfig {
    fn default() -> Self {
        AccountConfig {
            user: "cli".into(),
            setup_flag: "/etc/ethernet-switch-os/setup-required".into(),
            set_password: "/usr/sbin/ethernet-switch-os-set-password".into(),
            factory_reset: "/usr/sbin/ethernet-switch-os-factory-reset".into(),
            shadow: "/etc/shadow".into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AccountError {
    /// The new password is not acceptable.
    Invalid(String),
    /// The current password is missing or wrong.
    Denied(String),
    /// Anything else.
    Failed(String),
}

impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountError::Invalid(m) | AccountError::Denied(m) | AccountError::Failed(m) => {
                f.write_str(m)
            }
        }
    }
}

impl std::error::Error for AccountError {}

impl From<io::Error> for AccountError {
    fn from(e: io::Error) -> Self {
        AccountError::Failed(e.to_string())
    }
}

/// Checks a new password against the rules: length, and no control
/// characters, which chpasswd's line format cannot carry and a terminal
/// cannot type reliably.
pub fn validate_password(password: &str) -> Result<(), AccountError> {
    let len = password.chars().count();
    if len < MIN_PASSWORD_LEN {
        return Err(AccountError::Invalid(format!(
            "the password must have at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    if len > MAX_PASSWORD_LEN {
        return Err(AccountError::Invalid(format!(
            "the password must have at most {MAX_PASSWORD_LEN} characters"
        )));
    }
    if password.chars().any(char::is_control) {
        return Err(AccountError::Invalid(
            "the password must not contain control characters".into(),
        ));
    }
    Ok(())
}

/// The password hash of `user` in the text of /etc/shadow. None if the user
/// is missing; an empty string if the account has no password.
pub fn shadow_hash<'a>(shadow: &'a str, user: &str) -> Option<&'a str> {
    shadow.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        (fields.next() == Some(user)).then(|| fields.next().unwrap_or(""))
    })
}

#[cfg_attr(target_env = "gnu", link(name = "crypt"))]
extern "C" {
    // musl has crypt_r(3) in libc, glibc in libcrypt (libxcrypt).
    fn crypt_r(key: *const c_char, salt: *const c_char, data: *mut c_void) -> *mut c_char;
}

/// At least `struct crypt_data`: 260 bytes in musl, 32768 in libxcrypt.
/// Zeroed, as crypt_r wants it before the first call.
const CRYPT_DATA_SIZE: usize = 65536;

/// Whether `password` hashes to `hash`, a crypt(3) hash such as `$6$...`.
/// A locked (`!`, `*`) or empty hash never matches.
pub fn password_matches(password: &str, hash: &str) -> bool {
    if hash.is_empty() || hash.starts_with(['!', '*']) {
        return false;
    }
    let (Ok(key), Ok(salt)) = (CString::new(password), CString::new(hash)) else {
        return false;
    };
    let mut data = vec![0u64; CRYPT_DATA_SIZE / 8];
    let result = unsafe { crypt_r(key.as_ptr(), salt.as_ptr(), data.as_mut_ptr().cast()) };
    if result.is_null() {
        return false;
    }
    let computed = unsafe { CStr::from_ptr(result) }.to_bytes();
    // Compare every byte, so the time taken does not tell how many matched.
    computed.len() == hash.len()
        && computed
            .iter()
            .zip(hash.as_bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

impl AccountConfig {
    /// Whether the first-login setup is still pending.
    pub fn setup_required(&self) -> bool {
        self.setup_flag.exists()
    }

    /// Sets the password. `current` is needed unless the setup is pending.
    pub fn set_password(&self, current: Option<&str>, new: &str) -> Result<(), AccountError> {
        if !self.setup_required() {
            let current = current
                .ok_or_else(|| AccountError::Denied("the current password is required".into()))?;
            let shadow = std::fs::read_to_string(&self.shadow)?;
            let hash = shadow_hash(&shadow, &self.user)
                .ok_or_else(|| AccountError::Failed(format!("there is no user {}", self.user)))?;
            if !password_matches(current, hash) {
                return Err(AccountError::Denied("the current password is wrong".into()));
            }
        }
        validate_password(new)?;
        run_with_stdin(
            Command::new(&self.set_password).arg(&self.user),
            &format!("{new}\n"),
        )
    }

    /// Wipes the data partition on the next boot and reboots in the
    /// background, so that the caller still gets its reply out.
    pub fn factory_reset(&self) -> Result<(), AccountError> {
        run_with_stdin(Command::new(&self.factory_reset).arg("--later"), "")
    }
}

/// Runs `command` with `input` on stdin; its output goes to the log. Fails
/// with its stderr if it exits unsuccessfully.
fn run_with_stdin(command: &mut Command, input: &str) -> Result<(), AccountError> {
    let program = Path::new(command.get_program()).display().to_string();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AccountError::Failed(format!("cannot run {program}: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AccountError::Failed(format!(
            "{program} failed: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // "password" with salt "saltsalt", made with `openssl passwd -6`.
    const HASH: &str = "$6$saltsalt$qFmFH.bQmmtXzyBY0s9v7Oicd2z4XSIecDzlB5KiA2/jctKu9YterLp8wwnSq.qc.eoxqOmSuNp2xS0ktL3nh/";

    #[test]
    fn validates_passwords() {
        assert!(validate_password("12345678").is_ok());
        assert!(validate_password("äöüäöüäö").is_ok());
        assert!(validate_password("with:colon and space").is_ok());
        assert!(matches!(
            validate_password("1234567"),
            Err(AccountError::Invalid(_))
        ));
        assert!(matches!(
            validate_password("12345678\n"),
            Err(AccountError::Invalid(_))
        ));
        assert!(validate_password(&"x".repeat(MAX_PASSWORD_LEN)).is_ok());
        assert!(validate_password(&"x".repeat(MAX_PASSWORD_LEN + 1)).is_err());
    }

    #[test]
    fn finds_shadow_hash() {
        let shadow =
            "root::20000:0:99999:7:::\ncli:$6$a$b:20000:0:99999:7:::\nclicon:!:20000::::::\n";
        assert_eq!(shadow_hash(shadow, "root"), Some(""));
        assert_eq!(shadow_hash(shadow, "cli"), Some("$6$a$b"));
        assert_eq!(shadow_hash(shadow, "clicon"), Some("!"));
        assert_eq!(shadow_hash(shadow, "cl"), None);
        assert_eq!(shadow_hash(shadow, "nobody"), None);
    }

    #[test]
    fn matches_passwords() {
        // The firmware's hashes are bcrypt (`mkpasswd -m bcrypt -R 8`).
        let bcrypt = "$2b$08$abcdefghijklmnopqrstuub/LlVfC1A62K0uqLz37ReP6nxxZrYNe";
        assert!(password_matches("password", bcrypt));
        assert!(!password_matches("passwore", bcrypt));
        assert!(password_matches("password", HASH));
        assert!(!password_matches("Password", HASH));
        assert!(!password_matches("", ""));
        assert!(!password_matches("password", ""));
        assert!(!password_matches("password", "!"));
        assert!(!password_matches("password", &format!("!{HASH}")));
    }

    fn config(dir: &Path, setup: bool) -> AccountConfig {
        let flag = dir.join("setup-required");
        if setup {
            std::fs::write(&flag, "").unwrap();
        }
        let shadow = dir.join("shadow");
        std::fs::write(&shadow, format!("cli:{HASH}:20000:0:99999:7:::\n")).unwrap();
        let log = dir.join("log");
        let script = dir.join("set-password");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nread -r p\necho \"$1 $p\" > {}\n", log.display()),
        )
        .unwrap();
        let reset = dir.join("factory-reset");
        std::fs::write(
            &reset,
            format!("#!/bin/sh\necho \"reset $1\" > {}\n", log.display()),
        )
        .unwrap();
        for s in [&script, &reset] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(s, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        AccountConfig {
            user: "cli".into(),
            setup_flag: flag,
            set_password: script,
            factory_reset: reset,
            shadow,
        }
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("switch-net-account-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn setup_needs_no_current_password() {
        let dir = tempdir("setup");
        let c = config(&dir, true);
        assert!(c.setup_required());
        c.set_password(None, "new password").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("log")).unwrap(),
            "cli new password\n"
        );
    }

    #[test]
    fn change_needs_the_current_password() {
        let dir = tempdir("change");
        let c = config(&dir, false);
        assert!(!c.setup_required());
        assert!(matches!(
            c.set_password(None, "new password"),
            Err(AccountError::Denied(_))
        ));
        assert!(matches!(
            c.set_password(Some("wrong"), "new password"),
            Err(AccountError::Denied(_))
        ));
        assert!(!dir.join("log").exists());
        assert!(matches!(
            c.set_password(Some("password"), "short"),
            Err(AccountError::Invalid(_))
        ));
        c.set_password(Some("password"), "new password").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("log")).unwrap(),
            "cli new password\n"
        );
    }

    #[test]
    fn factory_reset_runs_the_script() {
        let dir = tempdir("reset");
        let c = config(&dir, false);
        c.factory_reset().unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("log")).unwrap(),
            "reset --later\n"
        );
    }

    #[test]
    fn reports_a_failing_script() {
        let dir = tempdir("fail");
        let mut c = config(&dir, true);
        c.set_password = dir.join("missing");
        assert!(matches!(
            c.set_password(None, "new password"),
            Err(AccountError::Failed(_))
        ));
    }
}
