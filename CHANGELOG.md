# Change Log

## [0.5.0] 2020-01-30

Major usability features and reorganization.

### Added
 - Admin dashboard interface
 - Users - actions, permissions, reports

### Changed
 - Frontend now tracks and auto-boots dispatcher and cache_worker
 -


## [0.4.2] 2019-18-09

This version of cortex was used to convert and bundle the 08.2019 version of the arXMLiv dataset. Mostly stability patches.

## [0.4.1] 2019-03-04

Minor polish of the newly released history reports, and related patches

## [0.4.0] 2019-01-04

CorTeX now has an automatic "historical runs" reporting capacity.

It provides insight into incremental changes in subsequent runs of a service over a corpus, helping to track both improvements and regressions, at a course-granular severity level.

See #41 for additional details.

## [0.3.2] 2019-21-01

Minor hygiene release: Update to latest Rust nightly (1.33) and Rocket (0.4).

## [0.3.1] 2018-18-09

Frontend upgrades, as well as stability fixes, for the successful conversion run of arXiv upto 08.2018 with the tex_to_html service.

## [0.3.0] 2018-16-08

Combined release changes upto 0.3.0, include:
 * pagination and dedicated preview URLs for task list reports
 * worker metadata tracking, as well as per-service worker reports
 * breaking changes to dispatcher API, as sink (zmq::PULL) replies are now also required to include an identity message, for better tracking


## [0.2.9] 2017-03-12

This release, detailed in PR #24 , is a major backend rewrite that ensures a more solid and maintainable foundation. This includes:

 * The postgresql backend is now realized entirely using the diesel ORM
 * The log messaging table has now been split into 5 tables - one per (latexml-convention) severity, in an effort to keep the final table sizes for billions of messages reasonable.
 * The new LogRecord trait makes that usable in Rust with moderate boilerplate, which I find acceptable.
 * The implications for the code base are more significant - there are large refactors in the backend APIs and coding style.
 * The code quality has been boosted by a more disciplined use of rustfmt and clippy.

The release has undergone a stress test of converting 1000 arXiv artciles and using the respective reports, as a basic sanity check.

## [0.2.0]  2017-07-08

This is a maintenance release that updates CorTeX to passing all tests on Rust Nightly 1.21 (2017-08-04).

### Added

 * URI escape guard for backslash (while pending a saner URI handling boilerplate)

### Changed

 * Frontend migrated from Nickel to Rocket. Templates now use Handlebars.
 * All major dependencies updated to latest stable releases (zeromq, postgres, hyper)
