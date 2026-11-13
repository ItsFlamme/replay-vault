//! The names the app uses in Windows (its startup entry and firewall rule) and what it may
//! change: whether it updates, changes Windows settings that are one per account, and may keep its
//! private OBS in a folder the user picked. Everything else asks here.

mod copy {
    use std::path::Path;

    /// The app, as the rest of the code asks about it.
    #[derive(Debug, PartialEq)]
    pub struct Instance;

    // The reasons the pages show beside an option that is turned off below (a place for
    // build-specific limits). Nothing is, so they are empty.
    /// For the update options (General).
    pub const UPDATES_OFF: &str = "";
    /// For Check for updates.
    pub const UPDATE_CHECK_OFF: &str = "";
    /// For options that change Windows settings (General, Keybinds).
    pub const TWEAKS_OFF: &str = "";
    /// For picking the private OBS's folder (Setup).
    pub const OBS_DIR_FIXED: &str = "";

    impl Instance {
        /// "Replay Vault".
        #[inline]
        pub const fn label(&self) -> &str {
            "Replay Vault"
        }

        /// The name of this copy's firewall rule for its private OBS.
        #[inline]
        pub const fn firewall_rule(&self) -> &str {
            "Replay Vault recorder"
        }

        /// Whether this copy may look at, move or remove what older versions named things ("OBS
        /// Instant Replay" startup entry, "Instant Replay recorder" rule).
        #[inline]
        pub const fn legacy_names_allowed(&self) -> bool {
            true
        }

        /// Whether this copy checks for, downloads and installs new versions.
        #[inline]
        pub const fn updates_allowed(&self) -> bool {
            true
        }

        /// Whether this copy may change Game Bar, NVIDIA's overlay and AMD Software's settings,
        /// which are one per Windows account. For the pages' boxes.
        #[inline]
        pub const fn windows_tweaks_allowed(&self) -> bool {
            true
        }

        /// Asked by tweaks.rs before it changes or puts back one of those settings.
        #[inline]
        pub const fn may_change_windows_settings(&self) -> bool {
            true
        }

        /// Whether this copy may keep its private OBS in a folder the user picked.
        #[inline]
        pub const fn obs_dir_allowed(&self) -> bool {
            true
        }

        /// Why Remove must leave the private OBS's install folder `root` as it is, or None to
        /// clear it.
        #[inline]
        pub fn install_root_kept(&self, _root: &Path) -> Option<String> {
            None
        }
    }

    /// The app.
    #[inline]
    pub fn current() -> &'static Instance {
        &Instance
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Nothing is turned off, so no page shows a reason.
        #[test]
        fn nothing_turned_off() {
            assert_eq!(*current(), Instance);
            assert_eq!(Instance.install_root_kept(Path::new(r"C:\Users\You\AppData\Local\replay-vault")), None);
            assert_eq!((UPDATES_OFF, UPDATE_CHECK_OFF, TWEAKS_OFF, OBS_DIR_FIXED), ("", "", "", ""));
        }
    }
}

pub use copy::*;

impl Instance {
    /// The name of this copy's Start with Windows entry.
    #[inline]
    pub fn run_value(&self) -> &str {
        self.label()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installed and portable copies keep exactly the names they always had, so nothing needs
    /// moving, and may do everything.
    #[test]
    fn names_and_capabilities() {
        let i = current();
        assert_eq!(i.run_value(), "Replay Vault");
        assert_eq!(i.label(), "Replay Vault");
        assert_eq!(i.firewall_rule(), "Replay Vault recorder");
        assert!(i.legacy_names_allowed() && i.updates_allowed() && i.windows_tweaks_allowed() && i.may_change_windows_settings() && i.obs_dir_allowed());
    }
}
