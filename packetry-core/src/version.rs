//! Version information.

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
