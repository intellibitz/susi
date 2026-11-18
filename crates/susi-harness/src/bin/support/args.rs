//! Shared decoding for the standalone harness frontends. Each keeps its own
//! command grammar, defaults and validation order.
// Each binary includes this module and uses a different subset of the helpers.
#![allow(dead_code, unused_macros, unused_imports)]

pub fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

pub fn parse_value<T: std::str::FromStr>(value: String, error: &str) -> Result<T, String> {
    value.parse().map_err(|_| error.to_owned())
}

pub fn number<T: std::str::FromStr>(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<T, String> {
    take(args, flag)?.parse().map_err(|_| {
        let unit = if flag == "--timeout" {
            " of seconds"
        } else {
            ""
        };
        format!("{} must be a number{unit}", flag.trim_start_matches("--"))
    })
}

macro_rules! required {
    ($($value:expr),+ => $error:literal) => {
        if $($value.is_empty())||+ {
            return Err($error.into());
        }
    };
}
pub(crate) use required;
