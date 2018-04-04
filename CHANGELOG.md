# Change Log

## [0.2.1 (in active dev)]

* Minimum required `rustc` is `1.27.0-nightly 2018-04-03`.

### Added
 * Toggle-able reports for all tasks, as well as per-severity tasks
 * Improved robustness of unpacking .gz sources on import
 * Fixed recognition of invalid tasks

### Changed
 * Switched to using Tera templates instead of Handlebars, due to performance limitations for large HTML tables

## [0.2.0]

This is a maintenance release that updates CorTeX to passing all tests on Rust Nightly 1.21 (2017-08-04).

### Added

 * URI escape guard for backslash (while pending a saner URI handling boilerplate)

### Changed

 * Frontend migrated from Nickel to Rocket. Templates now use Handlebars.
 * All major dependencies updated to latest stable releases (zeromq, postgres, hyper)
