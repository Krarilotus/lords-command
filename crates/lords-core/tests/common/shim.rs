//! The few sync calls the ownership tests make, in one place, so the same tests also run against the commit the
//! second Sol review examined (83f450e), whose API differed: there this file is replaced by its own version.
#![allow(dead_code)]

use super::Fixture;
use lords_core::sync::{self, Plan};
use std::path::{Path, PathBuf};

pub fn plan_all(f: &Fixture) -> Plan {
    sync::plan(&f.stacks(), &f.opts()).unwrap()
}

/// Applies a plan; the backup dir of the owner files it changed, if any.
pub fn apply_plan(_f: &Fixture, p: &Plan) -> Result<Option<PathBuf>, String> {
    sync::apply(p).map(|a| a.backups)
}

/// Applies an empty plan for the tool home `home`.
pub fn apply_nothing(_f: &Fixture, home: &Path) -> Result<(), String> {
    let p = Plan { home: home.to_path_buf(), ..Plan::default() };
    sync::apply(&p).map(|_| ())
}

pub fn restore_run(f: &Fixture, backups: &Path) -> Result<Vec<String>, String> {
    sync::restore(&sync::plan(&f.stacks(), &f.opts())?, backups)
}

pub fn import_apply(f: &Fixture) -> Result<(), String> {
    sync::import_and_apply(&f.pub_root(), Some(&f.priv_root()), &f.opts()).map(|_| ())
}

pub fn install_hooks(f: &Fixture, repo: &Path, force: bool) -> Result<(), String> {
    let exe = PathBuf::from("C:/lords/lords.exe");
    lords_core::hooks::install(repo, &exe, false, force, &f.home()).map(|_| ())
}
