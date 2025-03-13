mod client;
mod connection;
mod messages;

pub use client::TerminalClient;
pub use connection::{attempt_connection, establish_connection, SSHConnectParams};
pub use messages::{ClientMessage, ClientPayload, DataPayload, ServerMessage, ServerPayload};

pub const SSH_PING_INTERVAL_SECS: u64 = 10;
