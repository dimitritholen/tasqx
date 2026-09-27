//! Where the Secret Access Key lives: the OS keyring, or — for CI and
//! headless machines with no Secret Service — an environment variable that is
//! read on every call and never written anywhere.

/// Overrides the keyring for every verb when set. Never persisted.
pub const SECRET_ENV: &str = "TASQX_R2_SECRET_ACCESS_KEY";

/// The keyring service every entry of this connector is filed under.
pub const SERVICE: &str = "tasqx-remote-r2";

/// A place to keep one secret per user name. The OS keyring in the binary; a
/// fake in tests, so no test ever touches the real one.
pub trait Store {
    /// Save `secret` under `user`, replacing any earlier one.
    fn set(&self, user: &str, secret: &str) -> Result<(), String>;
    /// The secret under `user`; `Ok(None)` when there is none.
    fn get(&self, user: &str) -> Result<Option<String>, String>;
}

/// The platform keyring: Keychain, Credential Manager, or the Secret Service.
pub struct Keyring;

impl Store for Keyring {
    fn set(&self, user: &str, secret: &str) -> Result<(), String> {
        keyring::Entry::new(SERVICE, user)
            .and_then(|e| e.set_password(secret))
            .map_err(|e| e.to_string())
    }

    fn get(&self, user: &str) -> Result<Option<String>, String> {
        match keyring::Entry::new(SERVICE, user).and_then(|e| e.get_password()) {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Why the keyring could not be used, in words a user can act on.
fn unavailable(action: &str, detail: &str) -> String {
    format!(
        "cannot {action} the OS keyring ({detail}). On Linux this needs a running \
         Secret Service such as GNOME Keyring or KWallet; on a machine without one, \
         set {SECRET_ENV} in the environment tasqx runs in instead"
    )
}

/// Keep `secret` for `user`, unless the environment override is in use, in
/// which case nothing is stored at all.
pub fn save(store: &dyn Store, overridden: bool, user: &str, secret: &str) -> Result<(), String> {
    if overridden {
        return Ok(());
    }
    store
        .set(user, secret)
        .map_err(|e| unavailable("save the Secret Access Key in", &e))
}

/// The secret for `user`: the override when given, else the keyring's.
pub fn load(store: &dyn Store, overridden: Option<&str>, user: &str) -> Result<String, String> {
    if let Some(s) = overridden {
        return Ok(s.to_string());
    }
    match store.get(user) {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(format!(
            "the OS keyring holds no Secret Access Key for {user}; run configure again"
        )),
        Err(e) => Err(unavailable("read the Secret Access Key from", &e)),
    }
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::Store;

    /// An in-memory keyring, or one that is not there at all.
    #[derive(Default)]
    pub struct Fake {
        pub entries: RefCell<BTreeMap<String, String>>,
        pub broken: bool,
    }

    impl Store for Fake {
        fn set(&self, user: &str, secret: &str) -> Result<(), String> {
            if self.broken {
                return Err("no Secret Service on the session bus".into());
            }
            self.entries
                .borrow_mut()
                .insert(user.to_string(), secret.to_string());
            Ok(())
        }

        fn get(&self, user: &str) -> Result<Option<String>, String> {
            if self.broken {
                return Err("no Secret Service on the session bus".into());
            }
            Ok(self.entries.borrow().get(user).cloned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    #[test]
    fn a_saved_secret_loads_back() {
        let store = Fake::default();
        save(&store, false, "acct/bucket/key", "s3cret").unwrap();
        assert_eq!(load(&store, None, "acct/bucket/key").unwrap(), "s3cret");
    }

    #[test]
    fn an_unavailable_keyring_is_named_and_the_way_out_given() {
        let store = Fake {
            broken: true,
            ..Fake::default()
        };
        let msg = save(&store, false, "u", "s").unwrap_err();
        assert!(msg.contains("OS keyring"), "{msg}");
        assert!(msg.contains("no Secret Service"), "{msg}");
        assert!(msg.contains(SECRET_ENV), "{msg}");
        let msg = load(&store, None, "u").unwrap_err();
        assert!(msg.contains(SECRET_ENV), "{msg}");
    }

    #[test]
    fn the_override_is_never_stored() {
        let store = Fake {
            broken: true,
            ..Fake::default()
        };
        save(&store, true, "u", "s").unwrap();
        assert!(store.entries.borrow().is_empty());
        assert_eq!(load(&store, Some("from-env"), "u").unwrap(), "from-env");
    }

    #[test]
    fn a_missing_entry_says_to_configure_again() {
        let msg = load(&Fake::default(), None, "u").unwrap_err();
        assert!(msg.contains("run configure again"), "{msg}");
    }
}
