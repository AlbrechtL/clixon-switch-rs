//! The admin account, its password, and the factory reset.
//!
//! A fresh switch has no admin account. The first-login setup creates it,
//! with a name the user chooses; from then on the account is the one with
//! UID [`ADMIN_UID`], whatever its name. The password lives in /etc/shadow.
//! Creating the account, setting the password and the factory reset are
//! done by scripts of the firmware (see [`AccountConfig`]), so that root on
//! the serial console runs the same code as the RPCs. This module decides
//! whether a request may go ahead and checks names and passwords.

use std::ffi::{c_char, c_void, CStr, CString};
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Shortest password accepted.
pub const MIN_PASSWORD_LEN: usize = 8;
/// Longest password accepted: more is a mistake, not a password.
pub const MAX_PASSWORD_LEN: usize = 128;
/// Longest username accepted, as useradd's default.
pub const MAX_USERNAME_LEN: usize = 32;
/// The UID of the admin account, which the setup creates.
pub const ADMIN_UID: u32 = 1000;

/// Where the firmware's pieces are.
#[derive(Debug, Clone)]
pub struct AccountConfig {
    /// The UID of the admin account.
    pub uid: u32,
    /// Exists while there is no admin account: first-login setup.
    pub setup_flag: PathBuf,
    /// `set-password [--username NAME]`, reads the new password on stdin.
    /// With --username it creates the admin account (the setup), without
    /// it changes the password of the existing one.
    pub set_password: PathBuf,
    /// `factory-reset --later`: wipes the data partition on the next boot
    /// and reboots in the background.
    pub factory_reset: PathBuf,
    pub passwd: PathBuf,
    pub group: PathBuf,
    pub shadow: PathBuf,
}

impl Default for AccountConfig {
    fn default() -> Self {
        AccountConfig {
            uid: ADMIN_UID,
            setup_flag: "/etc/ethernet-switch-os/setup-required".into(),
            set_password: "/usr/sbin/ethernet-switch-os-set-password".into(),
            factory_reset: "/usr/sbin/ethernet-switch-os-factory-reset".into(),
            passwd: "/etc/passwd".into(),
            group: "/etc/group".into(),
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

/// Checks the name for a new account: a lower case letter or `_`, then up to
/// 31 lower case letters, digits, `_` and `-`. The portable subset that
/// useradd, login, dropbear and htpasswd (no `:`) all take.
pub fn validate_username(name: &str) -> Result<(), AccountError> {
    let valid = name.len() <= MAX_USERNAME_LEN
        && name
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-');
    if valid {
        Ok(())
    } else {
        Err(AccountError::Invalid(format!(
            "the username must start with a lower case letter or _, followed by \
             at most {} lower case letters, digits, _ or -",
            MAX_USERNAME_LEN - 1
        )))
    }
}

/// The names in /etc/passwd or /etc/group: the first field of each line.
fn names(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter_map(|line| line.split(':').next())
        .filter(|name| !name.is_empty())
}

/// The name of the user with `uid` in the text of /etc/passwd.
pub fn passwd_name(passwd: &str, uid: u32) -> Option<&str> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        (fields.nth(1)?.parse() == Ok(uid)).then_some(name)
    })
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

    /// The name of the admin account.
    pub fn admin_name(&self) -> Result<String, AccountError> {
        let passwd = std::fs::read_to_string(&self.passwd)?;
        passwd_name(&passwd, self.uid)
            .map(str::to_string)
            .ok_or_else(|| AccountError::Failed(format!("there is no user with UID {}", self.uid)))
    }

    /// Checks that `name` is free for the admin account: no user or group
    /// has it yet.
    fn check_name_free(&self, name: &str) -> Result<(), AccountError> {
        for file in [&self.passwd, &self.group] {
            if names(&std::fs::read_to_string(file)?).any(|n| n == name) {
                return Err(AccountError::Invalid(format!(
                    "the name {name} is taken by a system account"
                )));
            }
        }
        Ok(())
    }

    /// While the setup is pending: creates the admin account `username`
    /// with the password `new`. Afterwards: changes the admin password,
    /// which needs the `current` one, and takes no `username`.
    pub fn set_password(
        &self,
        username: Option<&str>,
        current: Option<&str>,
        new: &str,
    ) -> Result<(), AccountError> {
        let mut command = Command::new(&self.set_password);
        if self.setup_required() {
            let username = username.ok_or_else(|| {
                AccountError::Invalid("the username of the admin account is required".into())
            })?;
            validate_username(username)?;
            self.check_name_free(username)?;
            command.arg("--username").arg(username);
        } else {
            if username.is_some() {
                return Err(AccountError::Invalid(
                    "the admin account exists: its username is chosen once, during \
                     the setup, and stays until a factory reset"
                        .into(),
                ));
            }
            let current = current
                .ok_or_else(|| AccountError::Denied("the current password is required".into()))?;
            let user = self.admin_name()?;
            let shadow = std::fs::read_to_string(&self.shadow)?;
            let hash = shadow_hash(&shadow, &user)
                .ok_or_else(|| AccountError::Failed(format!("{user} is not in the shadow file")))?;
            if !password_matches(current, hash) {
                return Err(AccountError::Denied("the current password is wrong".into()));
            }
        }
        validate_password(new)?;
        run_with_stdin(&mut command, &format!("{new}\n"))
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
    fn validates_usernames() {
        for name in ["ops", "_x", "a-b_1", "x", &"a".repeat(MAX_USERNAME_LEN)] {
            assert!(validate_username(name).is_ok(), "{name}");
        }
        for name in [
            "",
            "Admin",
            "1abc",
            "-x",
            "a:b",
            "a b",
            "a.b",
            "ä",
            "ops\n",
            &"a".repeat(MAX_USERNAME_LEN + 1),
        ] {
            assert!(
                matches!(validate_username(name), Err(AccountError::Invalid(_))),
                "{name:?}"
            );
        }
    }

    #[test]
    fn finds_names_by_uid() {
        let passwd = "root:x:0:0:root:/root:/bin/sh\n\
                      clicon:x:999:999::/:/bin/false\n\
                      ops:x:1000:100::/home/ops:/usr/bin/ethernet-switch-os-cli\n";
        assert_eq!(passwd_name(passwd, 0), Some("root"));
        assert_eq!(passwd_name(passwd, 1000), Some("ops"));
        assert_eq!(passwd_name(passwd, 100), None);
        assert_eq!(passwd_name("broken\n", 0), None);
        assert_eq!(
            names("root:x:0:\n\nusers:x:100:\n").collect::<Vec<_>>(),
            ["root", "users"]
        );
    }

    #[test]
    fn finds_shadow_hash() {
        let shadow =
            "root::20000:0:99999:7:::\nops:$6$a$b:20000:0:99999:7:::\nclicon:!:20000::::::\n";
        assert_eq!(shadow_hash(shadow, "root"), Some(""));
        assert_eq!(shadow_hash(shadow, "ops"), Some("$6$a$b"));
        assert_eq!(shadow_hash(shadow, "clicon"), Some("!"));
        assert_eq!(shadow_hash(shadow, "op"), None);
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

    /// A switch before the setup (no admin account, `setup`) or after it
    /// (admin "ops" with the password "password").
    fn config(dir: &Path, setup: bool) -> AccountConfig {
        let flag = dir.join("setup-required");
        let passwd = dir.join("passwd");
        let group = dir.join("group");
        let shadow = dir.join("shadow");
        let mut passwd_text =
            "root:x:0:0:root:/root:/bin/sh\nclicon:x:999:999::/:/bin/false\n".to_string();
        let mut shadow_text = "root::20000:0:99999:7:::\nclicon:!:20000::::::\n".to_string();
        if setup {
            std::fs::write(&flag, "").unwrap();
        } else {
            passwd_text += "ops:x:1000:100::/home/ops:/usr/bin/ethernet-switch-os-cli\n";
            shadow_text += &format!("ops:{HASH}:20000:0:99999:7:::\n");
        }
        std::fs::write(&passwd, passwd_text).unwrap();
        std::fs::write(&shadow, shadow_text).unwrap();
        std::fs::write(&group, "root:x:0:\nusers:x:100:\nclicon:x:999:\n").unwrap();
        let log = dir.join("log");
        let script = dir.join("set-password");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nread -r p\necho \"$*|$p\" > {}\n", log.display()),
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
            uid: ADMIN_UID,
            setup_flag: flag,
            set_password: script,
            factory_reset: reset,
            passwd,
            group,
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
    fn setup_creates_the_account() {
        let dir = tempdir("setup");
        let c = config(&dir, true);
        assert!(c.setup_required());
        assert!(matches!(c.admin_name(), Err(AccountError::Failed(_))));
        // No current password needed, and one given is ignored.
        c.set_password(Some("ops"), Some("anything"), "new password")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("log")).unwrap(),
            "--username ops|new password\n"
        );
    }

    #[test]
    fn setup_needs_a_free_valid_username() {
        let dir = tempdir("setup-names");
        let c = config(&dir, true);
        assert!(matches!(
            c.set_password(None, None, "new password"),
            Err(AccountError::Invalid(_))
        ));
        // Users, a group without a user of that name, and malformed names.
        for name in ["root", "clicon", "users", "Ops", "1ops", "o:ps", ""] {
            assert!(
                matches!(
                    c.set_password(Some(name), None, "new password"),
                    Err(AccountError::Invalid(_))
                ),
                "{name:?}"
            );
        }
        assert!(matches!(
            c.set_password(Some("ops"), None, "short"),
            Err(AccountError::Invalid(_))
        ));
        assert!(!dir.join("log").exists());
    }

    #[test]
    fn change_needs_the_current_password() {
        let dir = tempdir("change");
        let c = config(&dir, false);
        assert!(!c.setup_required());
        assert_eq!(c.admin_name().unwrap(), "ops");
        assert!(matches!(
            c.set_password(None, None, "new password"),
            Err(AccountError::Denied(_))
        ));
        assert!(matches!(
            c.set_password(None, Some("wrong"), "new password"),
            Err(AccountError::Denied(_))
        ));
        assert!(!dir.join("log").exists());
        assert!(matches!(
            c.set_password(None, Some("password"), "short"),
            Err(AccountError::Invalid(_))
        ));
        c.set_password(None, Some("password"), "new password")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("log")).unwrap(),
            "|new password\n"
        );
    }

    #[test]
    fn username_only_during_setup() {
        let dir = tempdir("rename");
        let c = config(&dir, false);
        for name in ["ops", "other"] {
            assert!(matches!(
                c.set_password(Some(name), Some("password"), "new password"),
                Err(AccountError::Invalid(_))
            ));
        }
        assert!(!dir.join("log").exists());
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
            c.set_password(Some("ops"), None, "new password"),
            Err(AccountError::Failed(_))
        ));
    }
}
