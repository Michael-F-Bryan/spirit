use std::borrow::Borrow;
use std::fmt::{Debug, Display};
use std::mem;
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use failure::Error;
use futures::sync::{mpsc, oneshot};
use futures::Future;
use parking_lot::Mutex;
use serde::Deserialize;
use structopt::StructOpt;
use tk_listen::ListenExt;
use tokio;
use tokio::net::{TcpListener, TcpStream};
use tokio::prelude::*;
use tokio::reactor::Handle;

use super::super::validation::Result as ValidationResult;
use super::super::{Builder, Empty, Spirit, ValidationResults};
use super::{Helper, IteratedCfgHelper};

type BoxTask = Box<dyn Future<Item = (), Error = ()> + Send>;
// Should be Box<FnOnce>, but it doesn't work. So we wrap Option<FnOnce> into FnMut, pull it out
// and make sure not to call it twice (which would panic).
type TaskBuilder<T> = Box<dyn FnMut(&T) -> BoxTask + Send>;

pub(crate) struct TokioGutsInner<T> {
    tasks: Vec<TaskBuilder<T>>,
}

// TODO: Why can't derive?
impl<T> Default for TokioGutsInner<T> {
    fn default() -> Self {
        TokioGutsInner { tasks: Vec::new() }
    }
}

pub(crate) struct TokioGuts<T>(Mutex<TokioGutsInner<T>>);

impl<T> From<TokioGutsInner<T>> for TokioGuts<T> {
    fn from(inner: TokioGutsInner<T>) -> Self {
        TokioGuts(Mutex::new(inner))
    }
}

impl<S, O, C> Spirit<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Send + Sync + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync,
    O: StructOpt,
{
    pub fn tokio_spawn_tasks(me: &Arc<Self>) {
        let mut extracted = Vec::new();
        mem::swap(&mut extracted, &mut me.tokio_guts.0.lock().tasks);
        for mut task in extracted.drain(..) {
            tokio::spawn(task(me));
        }
    }
}

impl<S, O, C> Builder<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    pub fn tokio_task<F, Fut>(mut self, task: F) -> Self
    where
        F: FnOnce(&Arc<Spirit<S, O, C>>) -> Fut + Send + 'static,
        Fut: Future<Item = (), Error = ()> + Send + 'static,
    {
        let mut task = Some(task);
        // We effectively simulate FnOnce by FnMut that'd panic on another call
        let wrapper = move |spirit: &_| -> BoxTask {
            let task = task.take().unwrap();
            let fut = task(spirit);
            Box::new(fut)
        };
        self.tokio_guts.tasks.push(Box::new(wrapper));
        self
    }

    pub fn run_tokio(self) {
        self.run(|spirit| -> Result<(), Error> {
            tokio::run(future::lazy(move || {
                Spirit::tokio_spawn_tasks(&spirit);
                future::ok(())
            }));
            Ok(())
        })
    }
}

struct RemoteDrop {
    request_drop: Option<oneshot::Sender<()>>,
    drop_confirmed: Option<oneshot::Receiver<()>>,
}

impl Drop for RemoteDrop {
    fn drop(&mut self) {
        trace!("Requesting remote drop");
        // Ask the other side to drop the thing
        let _ = self.request_drop.take().unwrap().send(());
        // And wait for it to actually happen
        let _ = self.drop_confirmed.take().unwrap().wait();
        trace!("Remote drop done");
    }
}

pub struct Task<Extract, Build, ToTask, Name> {
    pub extract: Extract,
    pub build: Build,
    pub to_task: ToTask,
    pub name: Name,
}

impl Task<(), (), (), ()> {
    pub fn new() -> Self {
        Task {
            extract: (),
            build: (),
            to_task: (),
            name: (),
        }
    }
}

impl<Extract, Build, ToTask, Name> Task<Extract, Build, ToTask, Name> {
    pub fn with_extract<NewExtract>(
        self,
        extract: NewExtract,
    ) -> Task<NewExtract, Build, ToTask, Name> {
        Task {
            extract: extract,
            build: self.build,
            to_task: self.to_task,
            name: self.name,
        }
    }
    pub fn with_build<NewBuild>(self, build: NewBuild) -> Task<Extract, NewBuild, ToTask, Name> {
        Task {
            extract: self.extract,
            build: build,
            to_task: self.to_task,
            name: self.name,
        }
    }
    pub fn with_to_task<NewToTask>(
        self,
        to_task: NewToTask,
    ) -> Task<Extract, Build, NewToTask, Name> {
        Task {
            extract: self.extract,
            build: self.build,
            to_task: to_task,
            name: self.name,
        }
    }
    pub fn with_name<NewName>(self, name: NewName) -> Task<Extract, Build, ToTask, NewName> {
        Task {
            extract: self.extract,
            build: self.build,
            to_task: self.to_task,
            name: name,
        }
    }
}

impl<S, O, C, SubCfg, Resource, Extract, ExtractIt, ExtraCfg, Build, ToTask, InnerTask, Name>
    Helper<S, O, C> for Task<Extract, Build, ToTask, Name>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Extract: FnMut(&C) -> ExtractIt + Send + 'static,
    ExtractIt: IntoIterator<Item = (SubCfg, ExtraCfg, usize, ValidationResults)>,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    SubCfg: Clone + Debug + PartialEq + Send + 'static,
    Build: FnMut(&SubCfg) -> Result<Resource, Error> + Send + 'static,
    Resource: Clone + Send + 'static,
    ToTask: FnMut(&Arc<Spirit<S, O, C>>, Resource, ExtraCfg) -> InnerTask + Send + 'static,
    InnerTask: IntoFuture<Item = (), Error = Error> + Send + 'static,
    <InnerTask as IntoFuture>::Future: Send,
    Name: Clone + Display + Send + Sync + 'static,
{
    fn apply(self, builder: Builder<S, O, C>) -> Builder<S, O, C> {
        let Task {
            mut extract,
            mut build,
            mut to_task,
            name,
        } = self;
        debug!("Installing helper {}", name);
        // Note: this depends on the specific drop order to avoid races
        struct Install<R, ExtraCfg> {
            resource: R,
            drop_req: oneshot::Receiver<()>,
            confirm_drop: oneshot::Sender<()>,
            cfg: String,
            extra_conf: ExtraCfg,
        }
        #[derive(Clone)]
        struct Cache<SubCfg, ExtraCfg, Resource> {
            sub_cfg: SubCfg,
            extra_cfg: ExtraCfg,
            resource: Resource,
            remote: Vec<Arc<RemoteDrop>>,
        }
        let (install_sender, install_receiver) = mpsc::unbounded::<Install<Resource, ExtraCfg>>();

        let installer_name = name.clone();
        let installer = move |spirit: &Arc<Spirit<S, O, C>>| {
            let spirit = Arc::clone(spirit);
            install_receiver.for_each(move |install| {
                let Install {
                    resource,
                    drop_req,
                    confirm_drop,
                    cfg,
                    extra_conf,
                } = install;
                let name = installer_name.clone();
                debug!("Installing resource {} with config {}", name, cfg);
                // Get the task itself
                let task = to_task(&spirit, resource, extra_conf).into_future();
                let err_name = name.clone();
                let err_cfg = cfg.clone();
                // Wrap it in the cancelation routine
                let wrapped = task
                    .map_err(move |e| error!("Task {} on cfg {} failed: {}", err_name, err_cfg, e))
                    .select(drop_req.map_err(|_| ())) // Cancelation is OK too
                    .then(move |orig| {
                        debug!("Terminated resource {} on cfg {}", name, cfg);
                        drop(orig); // Make sure the original future is dropped first.
                        confirm_drop.send(())
                    })
                    .map_err(|_| ()); // If nobody waits for confirm_drop, that's OK.
                tokio::spawn(wrapped)
            })
        };

        let cache = Arc::new(Mutex::new(Vec::new()));
        let validator = move |_: &Arc<C>, cfg: &mut C, _: &O| -> ValidationResults {
            let mut results = ValidationResults::new();
            let orig_cache = cache.lock();
            let mut new_cache = Vec::new();
            let mut to_send = Vec::new();
            for sub in extract(cfg) {
                let (sub, extra, mut scale, sub_results) = sub;
                results.merge(sub_results);

                let previous = orig_cache
                    .iter()
                    .find(|Cache { sub_cfg, .. }| sub_cfg == &sub);

                let mut cached = if let Some(previous) = previous {
                    debug!("Reusing previous instance of {} for {:?}", name, sub);
                    previous.clone()
                } else {
                    trace!("Creating new instance of {} for {:?}", name, sub);
                    match build(&sub) {
                        Ok(resource) => {
                            debug!("Successfully created instance of {} for {:?}", name, sub);
                            Cache {
                                sub_cfg: sub.clone(),
                                extra_cfg: extra.clone(),
                                resource,
                                remote: Vec::new(),
                            }
                        }
                        Err(e) => {
                            let msg = format!("Creationg of {} for {:?} failed: {}", name, sub, e);
                            debug!("{}", msg); // The error will appear together for the validator
                            results.merge(ValidationResult::error(msg));
                            continue;
                        }
                    }
                };

                if extra != cached.extra_cfg {
                    debug!(
                        "Extra config for {:?} differs (old: {:?}, new: {:?}",
                        sub, cached.extra_cfg, extra
                    );
                    // If we have no old remotes here, they'll get dropped on installation and
                    // we'll „scale up“ to the current scale.
                    cached.remote.clear();
                }

                if cached.remote.len() > scale {
                    debug!(
                        "Scaling down {} from {} to {}",
                        name,
                        cached.remote.len(),
                        scale
                    );
                    cached.remote.drain(scale..);
                }

                if cached.remote.len() < scale {
                    debug!(
                        "Scaling up {} from {} to {}",
                        name,
                        cached.remote.len(),
                        scale
                    );
                    while cached.remote.len() < scale {
                        let (req_sender, req_recv) = oneshot::channel();
                        let (confirm_sender, confirm_recv) = oneshot::channel();
                        to_send.push(Install {
                            resource: cached.resource.clone(),
                            drop_req: req_recv,
                            confirm_drop: confirm_sender,
                            cfg: format!("{:?}", sub),
                            extra_conf: extra.clone(),
                        });
                        cached.remote.push(Arc::new(RemoteDrop {
                            request_drop: Some(req_sender),
                            drop_confirmed: Some(confirm_recv),
                        }));
                    }
                }

                new_cache.push(cached);
            }

            let sender = install_sender.clone();
            let cache = Arc::clone(&cache);
            let name = name.clone();
            results.merge(ValidationResult::nothing().on_success(move || {
                for install in to_send {
                    trace!("Sending {} to the reactor", install.cfg);
                    sender
                        .unbounded_send(install)
                        .expect("The tokio end got dropped");
                }
                *cache.lock() = new_cache;
                debug!("New version of {} sent", name);
            }));
            results
        };

        builder.config_validator(validator).tokio_task(installer)
    }
}

fn default_host() -> String {
    "::".to_owned()
}

fn default_scale() -> usize {
    1
}

fn default_error_sleep() -> u64 {
    100
}

fn default_max_conn() -> usize {
    1000
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Listen {
    port: u16,
    #[serde(default = "default_host")]
    host: String,
}

impl Listen {
    pub fn create_tcp(&self) -> Result<Arc<StdTcpListener>, Error> {
        Ok(Arc::new(StdTcpListener::bind((
            &self.host as &str,
            self.port,
        ))?))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TcpListen<ExtraCfg = Empty> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(default = "default_scale")]
    scale: usize,
    #[serde(rename = "error-sleep-ms", default = "default_error_sleep")]
    error_sleep_ms: u64,
    #[serde(rename = "max-conn", default = "default_max_conn")]
    max_conn: usize,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg: Clone + Debug + PartialEq + Send + 'static> TcpListen<ExtraCfg> {
    pub fn helper<Extract, ExtractIt, Conn, ConnFut, Name, S, O, C>(
        mut extract: Extract,
        conn: Conn,
        name: Name,
    ) -> impl Helper<S, O, C>
    where
        S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
        for<'de> C: Deserialize<'de> + Send + Sync + 'static,
        O: Debug + StructOpt + Sync + Send + 'static,
        Extract: FnMut(&C) -> ExtractIt + Send + 'static,
        ExtractIt: IntoIterator<Item = Self>,
        Conn: Fn(&Arc<Spirit<S, O, C>>, TcpStream, &ExtraCfg) -> ConnFut + Sync + Send + 'static,
        ConnFut: Future<Item = (), Error = Error> + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let conn = Arc::new(conn);

        let to_task_name = name.clone();
        let to_task =
            move |spirit: &Arc<Spirit<S, O, C>>,
                  listener: Arc<StdTcpListener>,
                  (cfg, error_sleep, max_conn): (ExtraCfg, Duration, usize)| {
                let spirit = Arc::clone(spirit);
                let conn = Arc::clone(&conn);
                let name = to_task_name.clone();
                listener
                    .try_clone() // Another copy of the listener
                    // std → tokio socket conversion
                    .and_then(|listener| TcpListener::from_std(listener, &Handle::default()))
                    .into_future()
                    .and_then(move |listener| {
                        listener.incoming()
                            // Handle errors like too many open FDs gracefully
                            .sleep_on_error(error_sleep)
                            .map(move |new_conn| {
                                let name = name.clone();
                                // The listen below keeps track of how many parallel connections
                                // there are. But it does so inside the same future, which prevents
                                // the separate connections to be handled in parallel on a thread
                                // pool. So we spawn the future to handle the connection itself.
                                // But we want to keep the future alive so the listen doesn't think
                                // it already terminated, therefore the done-channel.
                                let (done_send, done_recv) = oneshot::channel();
                                let handle_conn = conn(&spirit, new_conn, &cfg)
                                    .then(move |r| {
                                        if let Err(e) = r {
                                            error!("Failed to handle connection on {}: {}", name, e);
                                        }
                                        // Ignore the other side going away. This may happen if the
                                        // listener terminated, but the connection lingers for
                                        // longer.
                                        let _ = done_send.send(());
                                        future::ok(())
                                    });
                                tokio::spawn(handle_conn);
                                done_recv.then(|_| future::ok(()))
                            })
                            .listen(max_conn)
                            .map_err(|()| unreachable!("tk-listen never errors"))
                    }).map_err(Error::from)
            };

        let extract_name = name.clone();
        let extract = move |cfg: &C| {
            extract(cfg).into_iter().map(|c| {
                let (scale, results) = if c.scale > 0 {
                    (c.scale, ValidationResults::new())
                } else {
                    let msg = format!("Turning scale in {} from 0 to 1", extract_name);
                    (1, ValidationResult::warning(msg).into())
                };
                let sleep = Duration::from_millis(c.error_sleep_ms);
                (c.listen, (c.extra_cfg, sleep, c.max_conn), scale, results)
            })
        };

        Task {
            extract,
            build: Listen::create_tcp,
            to_task,
            name,
        }
    }
}

impl<S, O, C, Conn, ConnFut, ExtraCfg> IteratedCfgHelper<S, O, C, Conn> for TcpListen<ExtraCfg>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Conn: Fn(&Arc<Spirit<S, O, C>>, TcpStream, &ExtraCfg) -> ConnFut + Sync + Send + 'static,
    ConnFut: Future<Item = (), Error = Error> + Send + 'static,
{
    fn apply<Extractor, ExtractedIter, Name>(
        extractor: Extractor,
        action: Conn,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Self: Sized, // TODO: Why does rustc insist on this one?
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        Self::helper(extractor, action, name).apply(builder)
    }
}
