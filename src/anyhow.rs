//! Minimal pure-std CLI error and context plumbing for the `susi` binary,
//! replacing `susi-vendor-anyhow` with zero third-party dependencies and zero
//! `unsafe` code.

use std::fmt;

pub(crate) struct Error {
    msg: String,
}

impl Error {
    pub(crate) fn msg<M: fmt::Display>(message: M) -> Self {
        Self {
            msg: message.to_string(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl<E> From<E> for Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(err: E) -> Self {
        Self {
            msg: err.to_string(),
        }
    }
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

macro_rules! cli_anyhow {
    ($fmt:literal $($arg:tt)*) => {
        $crate::anyhow::Error::msg(format!($fmt $($arg)*))
    };
    ($err:expr $(,)?) => {
        $crate::anyhow::Error::msg($err)
    };
}

macro_rules! cli_bail {
    ($fmt:literal $($arg:tt)*) => {
        return ::core::result::Result::Err($crate::anyhow::Error::msg(format!($fmt $($arg)*)))
    };
    ($err:expr $(,)?) => {
        return ::core::result::Result::Err($crate::anyhow::Error::msg($err))
    };
}

pub(crate) use cli_anyhow as anyhow;
pub(crate) use cli_bail as bail;

#[cfg(test)]
mod tests {
    use super::{anyhow, bail, Error, Result};

    fn sample_bail(flag: bool) -> Result<&'static str> {
        if flag {
            bail!("bailing with code {}", 42);
        }
        Ok("ok")
    }

    #[test]
    fn errors_and_macros_format_cleanly() {
        let expr_err = anyhow!(String::from("custom failure"));
        assert_eq!(expr_err.to_string(), "custom failure");
        assert_eq!(Error::msg("plain").to_string(), "plain");

        assert_eq!(
            sample_bail(true).map_err(|e| e.to_string()),
            Err(String::from("bailing with code 42"))
        );
        assert_eq!(sample_bail(false).ok(), Some("ok"));
    }
}
