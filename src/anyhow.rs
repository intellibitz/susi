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

pub(crate) trait Context<T> {
    fn context<C: fmt::Display>(self, ctx: C) -> Result<T>;
    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> Result<T>;
}

impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context<C: fmt::Display>(self, ctx: C) -> Result<T> {
        self.map_err(|err| Error {
            msg: format!("{ctx}: {err}"),
        })
    }

    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> Result<T> {
        self.map_err(|err| Error {
            msg: format!("{}: {err}", f()),
        })
    }
}

impl<T> Context<T> for Option<T> {
    fn context<C: fmt::Display>(self, ctx: C) -> Result<T> {
        self.ok_or_else(|| Error::msg(ctx))
    }

    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> Result<T> {
        self.ok_or_else(|| Error::msg(f()))
    }
}

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
    use super::{anyhow, bail, Context, Error, Result};

    fn sample_bail(flag: bool) -> Result<&'static str> {
        if flag {
            bail!("bailing with code {}", 42);
        }
        Ok("ok")
    }

    #[test]
    fn error_from_std_context_option_and_macros_format_cleanly() {
        let io_res: std::io::Result<()> = Err(std::io::Error::other("disk full"));
        assert_eq!(
            io_res.context("write config").map_err(|e| e.to_string()),
            Err(String::from("write config: disk full"))
        );

        let none_val: Option<u32> = None;
        assert_eq!(
            none_val
                .with_context(|| "missing budget")
                .map_err(|e| e.to_string()),
            Err(String::from("missing budget"))
        );

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
