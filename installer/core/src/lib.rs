//! SpritzWine installer core.
//!
//! GUI-free logic for deploying a built, **relocatable** SpritzWine tree (a self-contained
//! `bin/ lib/ share/` tree plus the `spritzwine` launcher -- see `packaging/`) to the places
//! a user actually selects builds from:
//!
//! * **Steam** -- as a Proton-style Steam Play compatibility tool ([`install::install_steam`]),
//! * **Lutris** -- as a Wine runner ([`install::install_lutris`]),
//! * a **custom location** ([`install::install_custom`]),
//!
//! and optionally initialising a `WINEPREFIX` ([`install::init_prefix`]).
//!
//! Both front-ends -- the libcosmic GUI (`spritzwine-setup`) and the headless CLI
//! (`spritzwine-cli-install`) -- are thin shells over this crate. Everything here is pure and
//! synchronous so it can be unit-tested and driven from a background thread.

pub mod detect;
pub mod install;
pub mod steam;

pub use detect::{detect_lutris_roots, detect_steam_roots, LutrisRoot, SteamRoot};
pub use install::{
    detect_payload, init_prefix, install_custom, install_lutris, install_steam, validate_payload,
    Progress,
};

/// Default display/install name used for the Steam tool and Lutris runner directories.
pub const DEFAULT_NAME: &str = "SpritzWine";
