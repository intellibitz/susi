//! Synchronous HTTP client facade using native OS TLS.
//! Clients select NativeTls and PlatformVerifier explicitly; see the shared
//! susi-http-transport client builder. This facade has real upstream dependencies.
#![forbid(unsafe_code)]

pub use ureq::*;
