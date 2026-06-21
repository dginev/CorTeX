//! A ZMQ-based job dispatcher, interfacing between the task `Backend` and an open set of remote
//! workers

/// Finalize thread responsible for registering the returned tasks and messages in the database
pub mod finalize;
/// Manager orchestrating all dispatcher threads
pub mod manager;
/// Input-archive page-cache prefetcher (D-20)
pub mod prefetch;
/// Shared server utility functions between all dispatcher components
pub mod server;
/// Receiver ZMQ sink component
pub mod sink;
/// Emitter ZMQ ventilator component
pub mod ventilator;
