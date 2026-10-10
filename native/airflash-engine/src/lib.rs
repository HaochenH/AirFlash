pub mod auth;
pub mod clock;
#[cfg(windows)]
pub mod credentials;
#[cfg(not(windows))]
#[path = "credentials_unix.rs"]
pub mod credentials;
pub mod crypto;
pub mod discovery;
pub mod equalizer;
pub mod rtp;
pub mod rtsp;
pub mod session;
pub mod source;

#[cfg(windows)]
pub mod wasapi;

#[cfg(windows)]
pub mod live;
#[cfg(not(windows))]
#[path = "live_unix.rs"]
pub mod live;

#[cfg(all(unix, feature = "cli"))]
pub mod cli;

pub mod transport;
pub mod volume;
