//! Version information.

use std::fmt::Write;

use crate::built::*;

pub fn version() -> String {
   const MOD: &str = match GIT_DIRTY {
      Some(true) => "-modified",
      Some(false) | None => ""
   };

   match GIT_VERSION {
      None => PKG_VERSION.to_string(),
      Some(description) => format!("{PKG_VERSION} (git {description}{MOD})")
   }
}

pub fn version_info(with_dependencies: bool) -> String {
   let gtk_version = format!("{}.{}.{}",
      gtk::major_version(),
      gtk::minor_version(),
      gtk::micro_version());

   const COMMIT: &str = match GIT_COMMIT_HASH {
      Some(commit) => commit,
      None => "unknown"
   };

   const MODIFIED: &str = match GIT_DIRTY {
      Some(true) => " (modified)",
      Some(false) | None => ""
   };

   #[allow(clippy::const_is_empty)]
   const FEATURE_LIST: &str = if FEATURES.is_empty() {
      "(none)"
   } else {
      FEATURES_LOWERCASE_STR
   };

   const DEBUG_STR: &str = if DEBUG {"yes"} else {"no"};

   let output = format!("\
Runtime information:
  GTK version: {gtk_version}

Packetry build information:
  Git commit: {COMMIT}{MODIFIED}
  Cargo package version: {PKG_VERSION}
  Enabled features: {FEATURE_LIST}

Rust compiler:
  Version: {RUSTC_VERSION}
  Target: {TARGET} ({CFG_ENDIAN}-endian, {CFG_POINTER_WIDTH}-bit)
  Optimization level: {OPT_LEVEL}
  Debug build: {DEBUG_STR}");

  if with_dependencies {
     DEPENDENCIES
        .iter()
        .fold(
            format!("{output}\n\nBuilt with dependencies:"),
            |mut string, (pkg, ver)| {
                write!(string, "\n  {pkg} {ver}").unwrap();
                string
            }
        )
   } else {
      output
   }
}
