//! General purpose auxiliary routines that do not fit the MVC web service paradigm,
//! tending to minor tasks
use crate::backend::Backend;
use crate::frontend::params::{DashboardContext, FrontendConfig, TemplateContext};
use crate::models::{DaemonProcess, User};
use serde_json;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;

/// Provides default global fields for tera templates which may be needed at every page
pub fn global_defaults() -> HashMap<String, String> {
  let mut global = HashMap::new();
  global.insert(
    "google_oauth_id".to_owned(),
    dotenv!("GOOGLE_OAUTH_ID").to_owned(),
  );
  // dotenv!("GOOGLE_OAUTH_SECRET");
  global
}

/// Maps a cortex message severity into a bootstrap class for color highlight
pub fn severity_highlight(severity: &str) -> &str {
  match severity {
    // Bootstrap highlight classes
    "no_problem" => "success",
    "warning" => "warning",
    "error" => "error",
    "fatal" => "danger",
    "invalid" => "info",
    _ => "info",
  }
}
/// TODO: Is this outdated?
/// Maps a URI-encoded string into its regular plain text form
pub fn uri_unescape(param: Option<&str>) -> Option<String> {
  match param {
    None => None,
    Some(param_encoded) => {
      let mut param_decoded: String = param_encoded.to_owned();
      // TODO: This could/should be done faster by using lazy_static!
      for &(original, replacement) in &[
        ("%3A", ":"),
        ("%2F", "/"),
        ("%24", "$"),
        ("%2E", "."),
        ("%21", "!"),
        ("%40", "@"),
      ] {
        param_decoded = param_decoded.replace(original, replacement);
      }
      Some(
        percent_encoding::percent_decode(param_decoded.as_bytes())
          .decode_utf8_lossy()
          .into_owned(),
      )
    },
  }
}
/// TODO: Is this outdated?
/// Maps a regular string into a URI-encoded one
pub fn uri_escape(param: Option<String>) -> Option<String> {
  match param {
    None => None,
    Some(param_pure) => {
      let mut param_encoded: String =
        percent_encoding::utf8_percent_encode(&param_pure, percent_encoding::NON_ALPHANUMERIC)
          .collect::<String>();
      // TODO: This could/should be done faster by using lazy_static!
      for &(original, replacement) in &[
        (":", "%3A"),
        ("/", "%2F"),
        ("\\", "%5C"),
        ("$", "%24"),
        (".", "%2E"),
        ("!", "%21"),
        ("@", "%40"),
      ] {
        param_encoded = param_encoded.replace(original, replacement);
      }
      // if param_pure != param_encoded {
      //   println!("Encoded {:?} to {:?}", param_pure, param_encoded);
      // } else {
      //   println!("No encoding needed: {:?}", param_pure);
      // }
      Some(param_encoded)
    },
  }
}
/// Auto-generates a URI-encoded "foo_uri" entry for each "foo" label associated with a clickable
/// link (for Tera templates)
pub fn decorate_uri_encodings(context: &mut TemplateContext) {
  for inner_vec in &mut [
    &mut context.corpora,
    &mut context.services,
    &mut context.entries,
    &mut context.categories,
    &mut context.whats,
  ] {
    if let Some(ref mut inner_vec_data) = **inner_vec {
      for subhash in inner_vec_data {
        let mut uri_decorations = vec![];
        for (subkey, subval) in subhash.iter() {
          uri_decorations.push((
            subkey.to_string() + "_uri",
            uri_escape(Some(subval.to_string())).unwrap(),
          ));
        }
        for (decoration_key, decoration_val) in uri_decorations {
          subhash.insert(decoration_key, decoration_val);
        }
      }
    }
  }
  // global is handled separately
  let mut uri_decorations = vec![];
  for (subkey, subval) in &context.global {
    uri_decorations.push((
      subkey.to_string() + "_uri",
      uri_escape(Some(subval.to_string())).unwrap(),
    ));
  }
  for (decoration_key, decoration_val) in uri_decorations {
    context.global.insert(decoration_key, decoration_val);
  }
  let mut current_link = String::new();
  {
    if let Some(corpus_name) = context.global.get("corpus_name_uri") {
      if let Some(service_name) = context.global.get("service_name_uri") {
        current_link = format!("/corpus/{}/{}/", corpus_name, service_name);
        if let Some(severity) = context.global.get("severity_uri") {
          current_link.push_str(severity);
          current_link.push('/');
          if let Some(category) = context.global.get("category_uri") {
            current_link.push_str(category);
            current_link.push('/');
            if let Some(what) = context.global.get("what_uri") {
              current_link.push_str(what);
            }
          }
        }
      }
    }
  }
  if !current_link.is_empty() {
    context
      .global
      .insert("current_link_uri".to_string(), current_link);
  }
}

/// Loads the global `FrontendConfig` from config.json
pub fn load_config() -> FrontendConfig {
  let mut config_file = match File::open("config.json") {
    Ok(cfg) => cfg,
    Err(e) => panic!(
      "You need a well-formed JSON config.json file to run the frontend. Error: {}",
      e
    ),
  };
  let mut config_buffer = String::new();
  match config_file.read_to_string(&mut config_buffer) {
    Ok(_) => {},
    Err(e) => panic!(
      "You need a well-formed JSON config.json file to run the frontend. Error: {}",
      e
    ),
  };

  match serde_json::from_str(&config_buffer) {
    Ok(decoded) => decoded,
    Err(e) => panic!(
      "You need a well-formed JSON config.json file to run the frontend. Error: {}",
      e
    ),
  }
}

/// Prepare the context for the admin dashboard
pub fn dashboard_context(
  backend: Backend,
  current_user: Option<User>,
  mut global: HashMap<String, String>,
) -> DashboardContext
{
  if let Err(e) = crate::sysinfo::report(&mut global) {
    println!("Sys report failed: {:?}", e);
  }

  DashboardContext {
    global,
    current_user: current_user.unwrap_or_default(),
    daemons: DaemonProcess::all(&backend.connection).unwrap_or_default(),
    corpora: backend.corpora(),
    services: backend.services(),
    users: backend.users(),
    ..DashboardContext::default()
  }
}
