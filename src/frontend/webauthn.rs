// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Passkey (**WebAuthn**) sign-in — the relying-party instance built from config
//! (`docs/WEBAUTHN_DESIGN.md`). This is the **foundation**: the configured
//! [`webauthn_rs::prelude::Webauthn`] relying party as Rocket managed state. The
//! registration/authentication ceremonies and the sign-in UI build on this in the following
//! increments.
//!
//! The relying party is the CorTeX server itself — no external IdP, no per-deployment app
//! registration. Passkeys are the convenient day-to-day human sign-in; the admin token
//! (`frontend::actor`) remains the bootstrap / break-glass / agent credential. Passkeys never block
//! the token path: a disabled or misconfigured relying party degrades to `None` (logged), and
//! sign-in still works via the token.

use std::sync::Arc;

use webauthn_rs::prelude::*;

use crate::config::WebauthnConfig;

/// The configured WebAuthn relying-party instance, shared as Rocket managed state. Present only
/// when passkeys are **enabled** and the relying party built successfully; absent ⇒ token sign-in
/// only.
pub struct WebauthnState {
  /// The relying-party instance (`Arc` so the ceremony handlers cheaply share one instance).
  pub webauthn: Arc<Webauthn>,
}

/// Builds the relying-party [`Webauthn`] from config, or returns `None` (logged, never panics) when
/// passkeys are disabled or the `rp_id`/`rp_origin` are invalid — token sign-in keeps working
/// either way (graceful degradation, the robustness mandate).
pub fn build_state(config: &WebauthnConfig) -> Option<WebauthnState> {
  if !config.enabled {
    return None;
  }
  let origin = match Url::parse(&config.rp_origin) {
    Ok(origin) => origin,
    Err(error) => {
      eprintln!(
        "-- webauthn: invalid rp_origin {:?}: {error} (passkeys disabled)",
        config.rp_origin
      );
      return None;
    },
  };
  match WebauthnBuilder::new(&config.rp_id, &origin)
    .map(|builder| builder.rp_name("CorTeX"))
    .and_then(|builder| builder.build())
  {
    Ok(webauthn) => Some(WebauthnState {
      webauthn: Arc::new(webauthn),
    }),
    Err(error) => {
      eprintln!(
        "-- webauthn: cannot build relying party (rp_id={:?}, rp_origin={:?}): {error} (passkeys \
         disabled)",
        config.rp_id, config.rp_origin
      );
      None
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn disabled_config_yields_no_state() {
    assert!(
      build_state(&WebauthnConfig::default()).is_none(),
      "the default config is disabled, so no relying party is built"
    );
  }

  #[test]
  fn localhost_config_builds_a_relying_party() {
    let config = WebauthnConfig {
      enabled: true,
      rp_id: "localhost".to_string(),
      rp_origin: "http://localhost:8000".to_string(),
    };
    assert!(
      build_state(&config).is_some(),
      "a valid localhost relying party builds"
    );
  }

  #[test]
  fn invalid_origin_degrades_to_none_not_panic() {
    let config = WebauthnConfig {
      enabled: true,
      rp_id: "localhost".to_string(),
      rp_origin: "not a url".to_string(),
    };
    assert!(
      build_state(&config).is_none(),
      "an invalid origin disables passkeys gracefully (token path keeps working), never panics"
    );
  }
}
