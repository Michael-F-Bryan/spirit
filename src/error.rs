//! Error handling utilities
//!
//! This module contains utilities around error handling, like convenient error logging and
//! creation of multi-level errors.

use failure::Error;
use itertools::Itertools;
use log::{debug, log, log_enabled, Level};

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

    /// Like [SingleLine][ErrorLogFormat::SingleLine], but without the backtrace.
    SingleLineWithoutBacktrace,

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
pub fn log_error(level: Level, target: &str, e: &Error, format: ErrorLogFormat) {
    // Note: one of the causes is the error itself
    match format {
        ErrorLogFormat::MultiLine => {
            for cause in e.iter_chain() {
                log!(target: target, level, "{}", cause);
            }
        }
        ErrorLogFormat::SingleLine | ErrorLogFormat::SingleLineWithoutBacktrace => {
            log!(target: target, level, "{}", e.iter_chain().join("; "));
        }
        _ => unreachable!("Non-exhaustive sentinel should not be used"),
    }
    if log_enabled!(Level::Debug) && format != ErrorLogFormat::SingleLineWithoutBacktrace {
        let bt = format!("{}", e.backtrace());
        if !bt.is_empty() {
            debug!(target: target, "{}", bt);
        }
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
/// let err = failure::err_msg("Something's broken");
///
/// log_error!(Warn, err);
/// ```
///
/// [`Error`]: failure::Error
/// [`log_error`]: fn@crate::error::log_error
#[macro_export]
macro_rules! log_error {
    ($level: ident, $descr: expr => $err: expr) => {
        $crate::log_error!(@SingleLineWithoutBacktrace, $level, $err.context($descr).into());
    };
    ($level: ident, $err: expr) => {
        $crate::log_error!(@SingleLineWithoutBacktrace, $level, $err);
    };
    (multi $level: ident, $descr: expr => $err: expr) => {
        $crate::log_error!(@MultiLine, $level, $err.context($descr).into());
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
/// # use failure::{Error, ResultExt};
/// # use spirit::error;
/// # fn try_to_do_stuff() -> Result<(), Error> { Ok(()) }
///
/// let result = error::log_errors(module_path!(), || {
///     try_to_do_stuff().context("Didn't manage to do stuff")?;
///     Ok(())
/// });
/// # let _result = result;
/// ```
pub fn log_errors<R, F>(target: &str, f: F) -> Result<R, Error>
where
    F: FnOnce() -> Result<R, Error>,
{
    let result = f();
    if let Err(ref e) = result {
        log_error(Level::Error, target, e, ErrorLogFormat::MultiLine);
    }
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn log_error_macro() {
        let err = failure::err_msg("A test error");
        log_error!(Debug, err);
        log_error!(Debug, &err);
        log_error!(Debug, err.context("Another level").into());
        let err = failure::err_msg("A test error");
        log_error!(Debug, "Another level" => err);
        let multi_err = failure::err_msg("A test error")
            .context("Another level")
            .into();
        log_error!(multi Info, multi_err);
    }
}
