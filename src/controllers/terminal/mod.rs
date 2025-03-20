mod client;
mod connection;
mod messages;

pub use client::TerminalClient;
pub use connection::SSHConnectParams;

pub const SSH_PING_INTERVAL_SECS: u64 = 10;
