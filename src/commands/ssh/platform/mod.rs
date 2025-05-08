#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(not(unix))]
mod windows;
#[cfg(not(unix))]
pub use windows::*;

#[derive(Debug)]
pub enum SessionTermination {
    /// Session has been successfully closed
    Complete,

    /// Shell initialization failed
    InitShellError(String),

    /// Error reading from stdin
    StdinError(String),

    /// Error sending data to the server
    SendError(String),

    /// Server error occurred
    ServerError(String),

    /// Connection to the server was closed unexpectedly
    ConnectionReset,
}

impl SessionTermination {
    pub fn exit_code(&self) -> i32 {
        match self {
            SessionTermination::Complete => 0,
            SessionTermination::InitShellError(_) => 1,
            SessionTermination::StdinError(_) => 2,
            SessionTermination::SendError(_) => 3,
            SessionTermination::ServerError(_) => 4,
            SessionTermination::ConnectionReset => 5,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            SessionTermination::Complete => "",
            SessionTermination::InitShellError(msg) => msg,
            SessionTermination::StdinError(msg) => msg,
            SessionTermination::SendError(msg) => msg,
            SessionTermination::ServerError(msg) => msg,
            SessionTermination::ConnectionReset => {
                "Connection to the server was closed unexpectedly"
            }
        }
    }

    pub fn is_error(&self) -> bool {
        match self {
            SessionTermination::Complete => false,
            _ => true,
        }
    }
}

pub fn parse_server_error(error: String) -> SessionTermination {
    if error.contains("Connection reset without closing handshake") {
        SessionTermination::ConnectionReset
    } else {
        SessionTermination::ServerError(error)
    }
}
