//! A ZMQ-based job dispatcher, interfacing between the task `Backend` and an open set of remote
//! workers

/// Finalize thread responsible for registering the returned tasks and messages in the database
pub mod finalize;
/// Manager orchestrating all dispatcher threads
pub mod manager;
/// Shared server utility functions between all dispatcher components
pub mod server;
/// Receiver ZMQ sink component
pub mod sink;
/// Fail-fast supervision for the dispatcher's async components (rationalization phase 5)
pub mod supervisor;
/// Emitter ZMQ ventilator component
pub mod ventilator;
