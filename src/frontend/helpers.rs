//! General purpose auxiliary routines that do not fit the MVC web service paradigm,
//! tending to minor tasks
use crate::frontend::params::TemplateContext;

/// The "generated at" timestamp shown in report footers: the server's local date and time **to the
/// minute**, suffixed with the time-zone *abbreviation* — e.g. `Sat, 13 Jun 2026 22:57 EDT`.
///
/// chrono's `%Z` only renders the numeric UTC offset (`-04:00`) for `Local`, so the abbreviation
/// comes from the C library's `strftime %Z`, which reads the OS time-zone database and is
/// DST-correct. Falls back to chrono's offset rendering if the platform yields no abbreviation, so
/// the zone is never blank.
pub fn report_timestamp() -> String {
  let now = chrono::Local::now();
  let abbrev = local_tz_abbrev();
  if abbrev.is_empty() {
    now.format("%a, %d %b %Y %H:%M %Z").to_string()
  } else {
    format!("{} {}", now.format("%a, %d %b %Y %H:%M"), abbrev)
  }
}

/// The local time-zone abbreviation (`EDT`, `EST`, `UTC`, …) from the C library's `strftime`, or an
/// empty string if unavailable — the standard, DST-correct source chrono does not expose for
/// `Local`.
fn local_tz_abbrev() -> String {
  // SAFETY: `localtime_r` (the reentrant variant) fully initializes the stack `tm`; `strftime`
  // writes at most `buf.len()` bytes and returns the count written (0 on overflow). No borrowed
  // pointers escape, and both calls are thread-safe.
  unsafe {
    let now: libc::time_t = libc::time(std::ptr::null_mut());
    let mut tm: libc::tm = std::mem::zeroed();
    if libc::localtime_r(&now, &mut tm).is_null() {
      return String::new();
    }
    let mut buf = [0u8; 16];
    let written = libc::strftime(
      buf.as_mut_ptr() as *mut libc::c_char,
      buf.len(),
      c"%Z".as_ptr(),
      &tm,
    );
    String::from_utf8_lossy(&buf[..written]).into_owned()
  }
}

/// Formats a UTC [`chrono::NaiveDateTime`] (how CorTeX stores every timestamp) as an RFC 3339 /
/// ISO 8601 string with an explicit `+00:00` offset, e.g. `2026-06-15T05:52:00+00:00`. This is the
/// machine-readable, zone-unambiguous form emitted in DTO time fields and `<time datetime="…">`
/// attributes: the browser ([`public/js/localtime.js`]) rewrites it to the viewer's local time
/// *with the zone code* (EST/EDT/…), and agents get a directly parseable timestamp. Replaces the
/// old zone-ambiguous `%Y-%m-%d %H:%M` rendering, which silently displayed UTC as if it were local.
pub fn iso_utc(time: chrono::NaiveDateTime) -> String {
  // Seconds precision (no sub-second microseconds) — cleaner in the datetime attribute and as the
  // JS-off fallback text, still a valid RFC 3339 timestamp.
  time
    .and_utc()
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Groups an integer into thousands with commas (`2820484` → `2,820,484`) for human-facing counts
/// (corpus document totals, fleet throughput) that can reach millions. Agents get the raw number;
/// only the rendered HTML is grouped.
pub fn group_thousands(n: i64) -> String {
  let digits = n.unsigned_abs().to_string();
  let mut grouped = String::new();
  for (i, ch) in digits.chars().enumerate() {
    if i > 0 && (digits.len() - i).is_multiple_of(3) {
      grouped.push(',');
    }
    grouped.push(ch);
  }
  if n < 0 {
    format!("-{grouped}")
  } else {
    grouped
  }
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
      // TODO: This could/should be done faster by hoisting the table into a `LazyLock`.
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
      // TODO: This could/should be done faster by hoisting the table into a `LazyLock`.
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
            uri_escape(Some(subval.to_string())).unwrap_or_default(),
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
      uri_escape(Some(subval.to_string())).unwrap_or_default(),
    ));
  }
  for (decoration_key, decoration_val) in uri_decorations {
    context.global.insert(decoration_key, decoration_val);
  }
  let mut current_link = String::new();
  {
    if let Some(corpus_name) = context.global.get("corpus_name_uri")
      && let Some(service_name) = context.global.get("service_name_uri")
    {
      current_link = format!("/corpus/{corpus_name}/{service_name}/");
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
  if !current_link.is_empty() {
    context
      .global
      .insert("current_link_uri".to_string(), current_link);
  }
}

#[cfg(test)]
mod tests {
  use super::group_thousands;

  #[test]
  fn group_thousands_inserts_separators() {
    assert_eq!(group_thousands(0), "0");
    assert_eq!(group_thousands(7), "7");
    assert_eq!(group_thousands(999), "999");
    assert_eq!(group_thousands(1000), "1,000");
    assert_eq!(group_thousands(12345), "12,345");
    assert_eq!(group_thousands(2_820_484), "2,820,484");
    assert_eq!(group_thousands(-1234), "-1,234");
  }
}
