#![doc(
    html_root_url = "https://docs.rs/spirit/0.1.0/spirit/",
    test(attr(deny(warnings)))
)]
// #![deny(missing_docs, warnings)] XXX

extern crate arc_swap;
extern crate config;
extern crate failure;
extern crate libc;
#[macro_use]
extern crate log;
extern crate log_panics;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate signal_hook;
#[macro_use]
extern crate structopt;

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use arc_swap::ArcSwap;
use config::{Config, Environment, File};
use failure::Error;
use serde::Deserialize;
use signal_hook::iterator::Signals;
use structopt::StructOpt;
use structopt::clap::App;

#[derive(Deserialize)]
struct ConfigWrapper<C> {
    #[serde(flatten)]
    config: C,
}

#[derive(Debug, StructOpt)]
struct CommonOpts {
    /// Don't go into background and output logs to stderr as well.
    #[structopt(short = "f", long = "foreground")]
    foreground: bool,

    /// Configuration files or directories to load.
    #[structopt(parse(from_os_str))]
    configs: Vec<PathBuf>,
}

#[derive(Debug)]
struct OptWrapper<O> {
    common: CommonOpts,
    other: O,
}

// Unfortunately, StructOpt doesn't like flatten with type parameter
// (https://github.com/TeXitoi/structopt/issues/128). It is not even trivial to do, since some of
// the very important functions are *not* part of the trait. So we do it manually ‒ we take the
// type parameter's clap definition and add our own into it.
impl<O> StructOpt for OptWrapper<O>
where
    O: Debug + StructOpt,
{
    fn clap<'a, 'b>() -> App<'a, 'b> {
        CommonOpts::augment_clap(O::clap())
    }

    fn from_clap(matches: &::structopt::clap::ArgMatches) -> Self {
        OptWrapper {
            common: StructOpt::from_clap(matches),
            other: StructOpt::from_clap(matches),
        }
    }
}

pub fn log_errors<R, F: FnOnce() -> Result<R, Error>>(f: F) -> Result<R, Error> {
    let result = f();
    if let Err(ref e) = result {
        // TODO: Nicer logging with everything
        error!("{}", e);
    }
    result
}

pub struct Spirit<S, O = (), C = ()>
where
    S: Borrow<ArcSwap<C>> + 'static,
{
    config: S,
    // TODO: Overrides from command line
    // TODO: Mode selection for directories
    // TODO: Default values for config
    config_files: Vec<PathBuf>,
    config_env: Option<String>,
    config_filter: Box<Fn(&Path) -> bool + Sync + Send>,
    config_hooks: Vec<Box<Fn(&Arc<C>) + Sync + Send>>,
    // TODO: Validation
    opts: O,
    sig_hooks: HashMap<libc::c_int, Vec<Box<Fn() + Sync + Send>>>,
    terminate: AtomicBool,
    terminate_hooks: Vec<Box<Fn() + Sync + Send>>,
}

impl<S, O, C> Spirit<S, O, C>
where
    S: Borrow<ArcSwap<C>> + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync,
    O: StructOpt,
{
    #[cfg_attr(feature = "cargo-clippy", allow(new_ret_no_self))]
    pub fn new(config: S) -> Builder<S, O, C> {
        Builder {
            config,
            config_default_paths: Vec::new(),
            config_env: None,
            config_hooks: Vec::new(),
            config_filter: Box::new(|_| true),
            opts: PhantomData,
            sig_hooks: HashMap::new(),
            terminate_hooks: Vec::new(),
        }
    }

    pub fn cmd_opts(&self) -> &O {
        &self.opts
    }

    pub fn config_reload(&self) -> Result<(), Error> {
        unimplemented!();
    }

    pub fn is_terminated(&self) -> bool {
        self.terminate.load(Ordering::Relaxed)
    }

    pub fn log_reinit(&self) -> Result<(), Error> {
        unimplemented!();
    }

    fn background(&self, signals: &Signals) {
        for signal in signals.forever() {
            let term = match signal {
                libc::SIGHUP => {
                    let _ = log_errors(|| self.config_reload());
                    let _ = log_errors(|| self.log_reinit());
                    false
                },
                libc::SIGTERM | libc::SIGINT | libc::SIGQUIT => {
                    for hook in &self.terminate_hooks {
                        hook();
                    }
                    self.terminate.store(true, Ordering::Relaxed);
                    true
                },
                // Some other signal, only for the hook benefit
                _ => false,
            };

            if let Some(hooks) = self.sig_hooks.get(&signal) {
                for hook in hooks {
                    hook();
                }
            }

            if term {
                return;
            }
        }
        unreachable!("Signals run forever");
    }

    fn load_config(&self) -> Result<ConfigWrapper<C>, Error> {
        let mut config = Config::new();
        // TODO: Defaults, if any are provided
        for path in &self.config_files {
            if path.is_file() {
                config.merge(File::from(path as &Path))?;
            } else if path.is_dir() {
                for entry in path.read_dir()? {
                    let entry = entry?;
                    let path = entry.path();
                    let meta = path.symlink_metadata()?;
                    if !meta.is_file() || !(self.config_filter)(&path) {
                        continue;
                    }
                    config.merge(File::from(path))?;
                }
            } else {
                // TODO
            }
        }
        if let Some(env_prefix) = self.config_env.as_ref() {
            config.merge(Environment::with_prefix(env_prefix))?;
        }
        // TODO: Command line overrides
        Ok(config.try_into()?)
    }

    fn invoke_config_hooks(&self) {
        let cfg = self.config.borrow().load();
        for hook in &self.config_hooks {
            hook(&cfg);
        }
    }

}

pub struct Builder<S, O, C> {
    config: S,
    config_default_paths: Vec<PathBuf>,
    config_env: Option<String>,
    config_hooks: Vec<Box<Fn(&Arc<C>) + Sync + Send>>,
    config_filter: Box<Fn(&Path) -> bool + Sync + Send>,
    opts: PhantomData<O>,
    sig_hooks: HashMap<libc::c_int, Vec<Box<Fn() + Sync + Send>>>,
    terminate_hooks: Vec<Box<Fn() + Sync + Send>>,
}

impl<S, O, C> Builder<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    pub fn build(self) -> Result<Arc<Spirit<S, O, C>>, Error> {
        log_panics::init();
        let opts = OptWrapper::<O>::from_args();
        let config_files = if opts.common.configs.is_empty() {
            self.config_default_paths
        } else {
            opts.common.configs
        };
        let interesting_signals = self.sig_hooks
            .keys()
            .chain(&[libc::SIGHUP, libc::SIGTERM, libc::SIGQUIT, libc::SIGINT])
            .cloned()
            .collect::<HashSet<_>>(); // Eliminate duplicates
        let spirit = Spirit {
            config: self.config,
            config_files,
            config_env: self.config_env,
            config_hooks: self.config_hooks,
            config_filter: self.config_filter,
            opts: opts.other,
            sig_hooks: self.sig_hooks,
            terminate: AtomicBool::new(false),
            terminate_hooks: self.terminate_hooks,
        };
        let signals = Signals::new(interesting_signals)?;
        let config = spirit.load_config()?;
        spirit.config.borrow().store(Arc::new(config.config));
        spirit.invoke_config_hooks();
        let spirit = Arc::new(spirit);
        let spirit_bc = Arc::clone(&spirit);
        thread::Builder::new()
            .name("spirit".to_owned())
            // TODO: Something about panics
            .spawn(move || spirit_bc.background(&signals))
            .unwrap(); // Could fail only if the name contained \0
        Ok(spirit)
    }

    pub fn config_default_paths<P, I>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let paths = paths.into_iter()
            .map(Into::into)
            .collect();
        Self {
            config_default_paths: paths,
            .. self
        }
    }

    pub fn config_env<E: Into<String>>(self, env: E) -> Self {
        Self {
            config_env: Some(env.into()),
            .. self
        }
    }

    pub fn config_ext<E: Into<OsString>>(self, ext: E) -> Self {
        let ext = ext.into();
        Self {
            config_filter: Box::new(move |path| path.extension() == Some(&ext)),
            .. self
        }
    }

    pub fn config_filter<F: Fn(&Path) -> bool + Sync + Send + 'static>(self, filter: F) -> Self {
        Self {
            config_filter: Box::new(filter),
            .. self
        }
    }

    pub fn on_config<F: Fn(&Arc<C>) + Sync + Send + 'static>(self, hook: F) -> Self {
        let mut hooks = self.config_hooks;
        hooks.push(Box::new(hook));
        Self {
            config_hooks: hooks,
            .. self
        }
    }

    pub fn on_signal<F: Fn() + Sync + Send + 'static>(self, signal: libc::c_int, hook: F) -> Self {
        let mut hooks = self.sig_hooks;
        hooks.entry(signal)
            .or_insert_with(Vec::new)
            .push(Box::new(hook));
        Self {
            sig_hooks: hooks,
            .. self
        }
    }

    pub fn on_terminate<F: Fn() + Sync + Send + 'static>(self, hook: F) -> Self {
        let mut hooks = self.terminate_hooks;
        hooks.push(Box::new(hook));
        Self {
            terminate_hooks: hooks,
            .. self
        }
    }
}

// TODO: Provide contexts for thisg
// TODO: Validation of config
// TODO: Logging
// TODO: Log-panics
// TODO: Mode without external config storage ‒ have it all inside
