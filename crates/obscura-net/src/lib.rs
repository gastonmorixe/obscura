pub mod blocklist;
pub mod client;
pub mod cookies;
pub mod encoding;
pub mod interceptor;
pub mod localstorage;
pub mod robots;
#[cfg(feature = "stealth")]
pub mod wreq_client;

pub use client::{ObscuraHttpClient, ObscuraNetError, RequestInfo, ResourceType, Response};
pub use cookies::{CookieInfo, CookieJar};
pub use encoding::{decode_non_html, decode_response};
pub use localstorage::{origin_of, LocalStorageStore};
pub use robots::RobotsCache;
pub use blocklist::is_blocked as is_tracker_blocked;
#[cfg(feature = "stealth")]
pub use wreq_client::{StealthHttpClient, STEALTH_USER_AGENT};
