//! Cache worker logic, for efficient expiration of outdated CorTeX reports
use super::task_report::task_report;
use crate::backend::Backend;
use redis::Commands;
use std::collections::HashMap;
use std::thread;
use std::time::Duration;

/// A standalone worker loop for invalidating stale cache entries, mostly for CorTeX's frontend
/// report pages
pub fn cache_worker() {
  let redis_client = match redis::Client::open("redis://127.0.0.1/") {
    Ok(client) => client,
    _ => panic!("Redis connection failed, please boot up redis and restart the frontend!"),
  };
  let mut redis_connection = match redis_client.get_connection() {
    Ok(conn) => conn,
    _ => panic!("Redis connection failed, please boot up redis and restart the frontend!"),
  };
  let mut queued_cache: HashMap<String, usize> = HashMap::new();
  loop {
    // Keep a fresh backend connection on each invalidation pass.
    let backend = Backend::default();
    let mut global_stub: HashMap<String, String> = HashMap::new();
    // each corpus+service (non-import)
    for corpus in &backend.corpora() {
      if let Ok(services) = corpus.select_services(&backend.connection) {
        for service in &services {
          if service.name == "import" {
            continue;
          }
          println!(
            "[cache worker] Examining corpus {:?}, service {:?}",
            corpus.name, service.name
          );
          // Pages we'll cache:
          let report = backend.progress_report(corpus, service);
          let zero: f64 = 0.0;
          let huge: usize = 999_999;
          let queued_count_f64: f64 =
            report.get("queued").unwrap_or(&zero) + report.get("todo").unwrap_or(&zero);
          let queued_count: usize = queued_count_f64 as usize;
          let key_base: String = corpus.id.to_string() + "_" + &service.id.to_string();
          // Only recompute the inner pages if we are seeing a change / first visit, on the top
          // corpus+service level
          if *queued_cache.get(&key_base).unwrap_or(&huge) != queued_count {
            println!("[cache worker] state changed, invalidating ...");
            // first cache the count for the next check:
            queued_cache.insert(key_base.clone(), queued_count);
            // each reported severity (fatal, warning, error)
            for severity in &["invalid", "fatal", "error", "warning", "no_problem", "info"] {
              // most importantly, DEL the key from Redis!
              let key_severity = key_base.clone() + "_" + severity;
              println!("[cache worker] DEL {key_severity:?}");
              redis_connection.del(key_severity.clone()).unwrap_or(());
              // also the combined-severity page for this category
              let key_severity_all = key_severity.clone() + "_all_messages";
              println!("[cache worker] DEL {key_severity_all:?}");
              redis_connection.del(key_severity_all.clone()).unwrap_or(());
              if "no_problem" == *severity {
                continue;
              }

              // cache category page
              thread::sleep(Duration::new(1, 0)); // Courtesy sleep of 1 second.
              let category_report = task_report(
                &mut global_stub,
                corpus,
                service,
                Some((*severity).to_string()),
                None,
                None,
                &None,
              );
              // for each category, cache the what page
              for cat_hash in &category_report {
                let string_empty = String::new();
                let category = cat_hash.get("name").unwrap_or(&string_empty);
                if category.is_empty() || (category == "total") {
                  continue;
                }

                let key_category = key_severity.clone() + "_" + category;
                println!("[cache worker] DEL {key_category:?}");
                redis_connection.del(key_category.clone()).unwrap_or(());
                // also the combined-severity page for this `what` class
                let key_category_all = key_category + "_all_messages";
                println!("[cache worker] DEL {key_category_all:?}");
                redis_connection.del(key_category_all.clone()).unwrap_or(());

                let _ = task_report(
                  &mut global_stub,
                  corpus,
                  service,
                  Some((*severity).to_string()),
                  Some((*category).to_string()),
                  None,
                  &None,
                );
              }
            }
          }
        }
      }
    }
    // Take two minutes before we recheck.
    thread::sleep(Duration::new(120, 0));
  }
}
