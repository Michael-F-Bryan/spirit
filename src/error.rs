//! Error handling utilities
//!
//! This module contains utilities around error handling, like convenient error logging and
//! creation of multi-level errors.

use std::error::Error;

use itertools::Itertools;
use log::{log, Level};

/// How to format errors in logs.
///
/// The enum is non-exhaustive ‒ more variants may be added in the future and it won't be
/// considered an API breaking change.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[allow(deprecated)] // We get a deprecated warning on the one variant we deprecate ourselves.
pub enum ErrorLogFormat {
    /// Multi-cause error will span multiple log messages.
    ///
    /// If present, trace is printed on debug level.
    MultiLine,

    /// The error is formatted on a single line.
    ///
    /// The causes are separated by semicolons.
    ///
    /// If present, trace is printed on debug level.
    SingleLine,

    // Prevent users from accidentally matching against this enum without a catch-all branch.
    #[doc(hidden)]
    #[allow(non_camel_case_types)]
    _NON_EXHAUSTIVE,
}

/// Log one error on given log level.
///
/// It is printed to the log with all the causes and optionally a backtrace (if it is available and
/// debug logging is enabled).
///
/// This is the low-level version with full customization. You might also be interested in
/// [`log_errors`] or one of the convenience macro ([`log_error`][macro@log_error]).
pub fn log_error(level: Level, target: &str, e: &dyn Error, format: ErrorLogFormat) {
    let mut chain = itertools::unfold(Some(e), |e| {
        let current = e.take();
        if let Some(current) = current {
            *e = current.source();
        }
        current
    });
    // Note: one of the causes is the error itself
    match format {
        ErrorLogFormat::MultiLine => {
            for cause in chain {
                log!(target: target, level, "{}", cause);
            }
        }
        ErrorLogFormat::SingleLine => {
            log!(target: target, level, "{}", chain.join("; "));
        }
        _ => unreachable!("Non-exhaustive sentinel should not be used"),
    }
}

/// A convenience macro to log an [`Error`].
///
/// This logs an [`Error`] on given log level as a single line without backtrace. Removes some
/// boilerplate from the [`log_error`] function.
///
/// # Examples
///
/// ```rust
/// use spirit::log_error;
///
/// let err = std::io::Error::last_os_error();
///
/// log_error!(Warn, err);
/// ```
///
/// [`Error`]: failure::Error
/// [`log_error`]: fn@crate::error::log_error
#[macro_export]
macro_rules! log_error {
    ($level: ident, $descr: expr => $err: expr) => {
        // XXX
        $crate::log_error!(@SingleLine, $level, $err.context($descr).compat());
    };
    ($level: ident, $err: expr) => {
        $crate::log_error!(@SingleLine, $level, $err);
    };
    (multi $level: ident, $descr: expr => $err: expr) => {
        // XXX
        $crate::log_error!(@MultiLine, $level, $err.context($descr).compat());
    };
    (multi $level: ident, $err: expr) => {
        $crate::log_error!(@MultiLine, $level, $err);
    };
    (@$format: ident, $level: ident, $err: expr) => {
        $crate::error::log_error(
            $crate::macro_support::Level::$level,
            module_path!(),
            &$err,
            $crate::error::ErrorLogFormat::$format,
        );
    };
}

// XXX Get rid of failure in this example
/// A wrapper around a fallible function, logging any returned errors.
///
/// The errors will be logged in the provided target. You may want to provide `module_path!` as the
/// target.
///
/// If the error has multiple levels (causes), they are printed in multi-line fashion, as multiple
/// separate log messages.
///
/// # Examples
///
/// ```rust
/// # use failure::{Compat, Error, ResultExt};
/// # use spirit::error;
/// # fn try_to_do_stuff() -> Result<(), Error> { Ok(()) }
///
/// let result = error::log_errors(module_path!(), || -> Result<(), Compat<_>> {
///     try_to_do_stuff().context("Didn't manage to do stuff").compat()?;
///     Ok(())
/// });
/// # let _result = result;
/// ```
pub fn log_errors<R, E, F>(target: &str, f: F) -> Result<R, E>
where
    F: FnOnce() -> Result<R, E>,
    E: Error,
{
    let result = f();
    if let Err(ref e) = result {
        log_error(Level::Error, target, e, ErrorLogFormat::MultiLine);
    }
    result
}

#[cfg(test)]
mod tests {
    use failure::Fail;

    #[test]
    fn log_error_macro() {
        let err = failure::err_msg("A test error").compat();
        log_error!(Debug, err);
        let err = err.context("Another level").compat();
        log_error!(Debug, err);
        let err = failure::err_msg("A test error");
        log_error!(Debug, "Another level" => err);
        let multi_err = failure::err_msg("A test error")
            .context("Another level")
            .compat();
        log_error!(multi Info, multi_err);
    }
}
