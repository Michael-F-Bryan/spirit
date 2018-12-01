#![doc(
    html_root_url = "https://docs.rs/spirit-hyper/0.2.0/spirit_hyper/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! [Spirit] helper for Hyper
//!
//! This allows having Hyper servers auto-spawned from configuration. It is possible to put them on
//! top of arbitrary stream-style IO objects (TcpStream, UdsStream, these wrapped in SSL...).
//!
//! # Examples
//!
//! ```rust
//! extern crate hyper;
//! extern crate serde;
//! #[macro_use]
//! extern crate serde_derive;
//! extern crate spirit;
//! extern crate spirit_hyper;
//! extern crate spirit_tokio;
//!
//! use hyper::{Body, Request, Response};
//! use spirit::{Empty, Spirit};
//! use spirit_hyper::HttpServer;
//!
//! const DEFAULT_CONFIG: &str = r#"
//! [server]
//! port = 1234
//! "#;
//!
//! #[derive(Default, Deserialize)]
//! struct Config {
//!     server: HttpServer,
//! }
//!
//! impl Config {
//!     fn server(&self) -> HttpServer {
//!         self.server.clone()
//!     }
//! }
//!
//! fn request(_req: Request<Body>) -> Response<Body> {
//!     Response::new(Body::from("Hello world\n"))
//! }
//!
//! fn main() {
//!     Spirit::<Empty, Config>::new()
//!         .config_defaults(DEFAULT_CONFIG)
//!         .with(spirit_tokio::resource(
//!             Config::server,
//!             spirit_hyper::server_ok(request),
//!             "server",
//!         ))
//!         .run(|spirit| {
//! #           let spirit = std::sync::Arc::clone(spirit);
//! #           std::thread::spawn(move || spirit.terminate());
//!             Ok(())
//!         });
//! }
//! ```
//!
//! Further examples are in the
//! [git repository](https://github.com/vorner/spirit/tree/master/spirit-hyper/examples).
//!
//! [Spirit]: https://crates.io/crates/spirit.

extern crate failure;
extern crate futures;
extern crate hyper;
#[macro_use]
extern crate log;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate spirit_tokio;
extern crate tokio;

use std::error::Error;
use std::sync::Arc;

use failure::Error as FailError;
use futures::sync::oneshot::{self, Sender};
use futures::{Async, Future, IntoFuture, Poll};
use hyper::body::Payload;
use hyper::server::Server;
use hyper::service::{MakeService, Service};
use hyper::{Body, Request, Response};
use spirit::validation::Results as ValidationResults;
use spirit::{Builder, Empty, Spirit};
use spirit_tokio::{
    ExtraCfgCarrier, IntoIncoming, Name, ResourceConfig, ResourceConsumer, TcpListen,
};
use tokio::io::{AsyncRead, AsyncWrite};

/// Used to signal the graceful shutdown to hyper server.
struct SendOnDrop(Option<Sender<()>>);

impl Drop for SendOnDrop {
    fn drop(&mut self) {
        let _ = self.0.take().unwrap().send(());
    }
}

impl Future for SendOnDrop {
    type Item = ();
    type Error = FailError;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        Ok(Async::NotReady)
    }
}

/// Factory for [`MakeService`] implementations.
///
/// Each HTTP connection needs its own [`Service`] instance. As the hyper server accepts the
/// connections, it uses the [`MakeService`] factory to create them.
///
/// The configuration needs to spawn whole new servers (each with its own [`MakeService`]).
/// Therefore, we introduce another level ‒ this trait. It is passed to the [`server`] function.
///
/// There's a blanket implementation for compatible closures.
///
/// There are also functions similar to [`server`] which forgo some flexibility in favor of
/// convenience.
pub trait ConfiguredMakeService<O, C, Cfg>: Send + Sync + 'static
where
    Cfg: ResourceConfig<O, C>,
{
    /// The type of `MakeService` created.
    type MakeService;

    /// Create a new `MakeService` instance.
    ///
    /// # Parameters
    ///
    /// * `spirit`: The spirit instance.
    /// * `cfg`: The configuration fragment that caused creation of the server.
    /// * `resource`: The acceptor (eg. `TcpListener::accept`) the server will use.
    /// * `name`: Logging name.
    fn make(
        &self,
        spirit: &Arc<Spirit<O, C>>,
        cfg: &Arc<Cfg>,
        resource: &Cfg::Resource,
        name: &str,
    ) -> Self::MakeService;
}

impl<O, C, Cfg, F, R> ConfiguredMakeService<O, C, Cfg> for F
where
    Cfg: ResourceConfig<O, C>,
    F: Fn(&Arc<Spirit<O, C>>, &Arc<Cfg>, &Cfg::Resource, &str) -> R + Send + Sync + 'static,
{
    type MakeService = R;
    fn make(
        &self,
        spirit: &Arc<Spirit<O, C>>,
        cfg: &Arc<Cfg>,
        resource: &Cfg::Resource,
        name: &str,
    ) -> R {
        self(spirit, cfg, resource, name)
    }
}

/// Creates a [`ResourceConsumer`] from a [`ConfiguredMakeService`].
///
/// This is the lowest level constructor of the hyper resource consumers, when the full flexibility
/// is needed.
///
/// # Examples
///
/// ```rust
/// extern crate hyper;
/// extern crate serde;
/// #[macro_use]
/// extern crate serde_derive;
/// extern crate spirit;
/// extern crate spirit_hyper;
/// extern crate spirit_tokio;
///
/// use hyper::{Body, Request, Response};
/// use spirit::{Empty, Spirit};
/// use spirit_hyper::HttpServer;
///
/// #[derive(Default, Deserialize)]
/// struct Config {
///     #[serde(default)]
///     server: Vec<HttpServer>,
/// }
///
/// impl Config {
///     fn server(&self) -> Vec<HttpServer> {
///         self.server.clone()
///     }
/// }
///
/// fn request(_req: Request<Body>) -> Response<Body> {
///     Response::new(Body::from("Hello world\n"))
/// }
///
/// fn main() {
///     Spirit::<Empty, Config>::new()
///         .with(spirit_tokio::resources(
///             Config::server,
///             spirit_hyper::server(|_spirit: &_, _cfg: &_, _resource: &_, _name: &str| {
///                 || hyper::service::service_fn_ok(request)
///             }),
///             "server",
///         ))
///         .run(|spirit| {
/// #           let spirit = std::sync::Arc::clone(spirit);
/// #           std::thread::spawn(move || spirit.terminate());
///             Ok(())
///         });
/// }
/// ```
pub fn server<R, O, C, CMS, B, E, ME, S, F>(
    configured_make_service: CMS,
) -> impl ResourceConsumer<HyperServer<R>, O, C>
where
    R: ResourceConfig<O, C>,
    R::Resource: IntoIncoming,
    <R::Resource as IntoIncoming>::Connection: AsyncRead + AsyncWrite,
    CMS: ConfiguredMakeService<O, C, HyperServer<R>>,
    // TODO: Ask hyper to make their MakeServiceRef public, this monster is ugly :-(.
    CMS::MakeService: for<'a> MakeService<
            &'a <R::Resource as IntoIncoming>::Connection,
            ReqBody = Body,
            Error = E,
            MakeError = ME,
            Service = S,
            Future = F,
            ResBody = B,
        > + Send
        + Sync
        + 'static,
    E: Into<Box<Error + Send + Sync>>,
    ME: Into<Box<Error + Send + Sync>>,
    S: Service<ReqBody = Body, ResBody = B, Error = E> + Send + 'static,
    S::Future: Send,
    F: Future<Item = S, Error = ME> + Send + 'static,
    B: Payload,
{
    move |spirit: &Arc<Spirit<O, C>>,
          config: &Arc<HyperServer<R>>,
          resource: R::Resource,
          name: &str| {
        let (sender, receiver) = oneshot::channel();
        debug!("Starting hyper server {}", name);
        let name_success = name.to_owned();
        let name_err = name.to_owned();
        let make_service = configured_make_service.make(spirit, config, &resource, name);
        let server = Server::builder(resource.into_incoming())
            .serve(make_service)
            .with_graceful_shutdown(receiver)
            .map(move |()| debug!("Hyper server {} shut down", name_success))
            .map_err(move |e| error!("Hyper server {} failed: {}", name_err, e));
        tokio::spawn(server);
        SendOnDrop(Some(sender))
    }
}

/// Creates a hyper [`ResourceConfig`] for a closure that returns the [`Response`] directly.
///
/// This is like [`server`], but the passed parameter is `Fn(Request) -> Response`. This means it
/// is not passed anything from `spirit`, it is synchronous and never fails. It must be cloneable.
pub fn server_ok<R, O, C, S, B>(service: S) -> impl ResourceConsumer<HyperServer<R>, O, C>
where
    R: ResourceConfig<O, C>,
    R::Resource: IntoIncoming,
    <R::Resource as IntoIncoming>::Connection: AsyncRead + AsyncWrite,
    S: Fn(Request<Body>) -> Response<B> + Clone + Send + Sync + 'static,
    B: Payload,
{
    let configure_service = move |_: &_, _: &_, _: &_, _: &_| {
        let service = service.clone();
        move || hyper::service::service_fn_ok(service.clone())
    };
    server(configure_service)
}

/// Creates a hyper [`ResourceConfig`] for a closure that returns a future of [`Response`].
///
/// This is like [`server`], but the passed parameter is
/// `Fn(Request) -> impl Future<Item = Response>`. This means it is not passed any configuration
/// from `spirit`. It also needs to be cloneable.
pub fn server_simple<R, O, C, S, Fut, B>(service: S) -> impl ResourceConsumer<HyperServer<R>, O, C>
where
    R: ResourceConfig<O, C>,
    R::Resource: IntoIncoming,
    <R::Resource as IntoIncoming>::Connection: AsyncRead + AsyncWrite,
    S: Fn(Request<Body>) -> Fut + Clone + Send + Sync + 'static,
    Fut: IntoFuture<Item = Response<B>> + Send + 'static,
    Fut::Future: Send + 'static,
    Fut::Error: Into<Box<Error + Send + Sync>>,
    B: Payload,
{
    let configure_service = move |_: &_, _: &_, _: &_, _: &_| {
        let service = service.clone();
        move || hyper::service::service_fn(service.clone())
    };
    server(configure_service)
}

/// Like [`server`], but taking a closure to answer request directly.
///
/// The closure taken is `Fn(spirit, cfg, request) -> impl Future<Response>`.
///
/// If the configuration is not needed, the [`server_simple`] or [`server_ok`] might be an
/// alternative.
///
/// # Examples
///
/// ```rust
/// extern crate hyper;
/// extern crate serde;
/// #[macro_use]
/// extern crate serde_derive;
/// extern crate spirit;
/// extern crate spirit_hyper;
/// extern crate spirit_tokio;
///
/// use std::collections::HashSet;
/// use std::sync::Arc;
///
/// use hyper::{Body, Request, Response};
/// use spirit::{Empty, Spirit};
/// use spirit_tokio::ExtraCfgCarrier;
/// use spirit_hyper::HttpServer;
///
/// const DEFAULT_CONFIG: &str = r#"
/// [[server]]
/// port = 3456
///
/// [ui]
/// msg = "Hello world"
/// "#;
///
///
/// #[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Hash)]
/// struct Signature {
///     signature: Option<String>,
/// }
///
/// #[derive(Default, Deserialize)]
/// struct Ui {
///     msg: String,
/// }
///
/// #[derive(Default, Deserialize)]
/// struct Config {
///     /// On which ports (and interfaces) to listen.
///     ///
///     /// With some additional configuration about listening, the http server...
///     ///
///     /// Also, signature of the given listening port.
///     #[serde(default)]
///     listen: HashSet<HttpServer<Signature>>,
///     /// The UI (there's only the message to send).
///     ui: Ui,
/// }
///
/// impl Config {
///     /// A function to extract the HTTP servers configuration
///     fn listen(&self) -> HashSet<HttpServer<Signature>> {
///         self.listen.clone()
///     }
/// }
///
/// fn hello(
///     spirit: &Arc<Spirit<Empty, Config>>,
///     cfg: &Arc<HttpServer<Signature>>,
///    _req: Request<Body>,
/// ) -> Result<Response<Body>, std::io::Error> {
///     // Get some global configuration
///     let mut msg = format!("{}\n", spirit.config().ui.msg);
///     // Get some listener-local configuration.
///     if let Some(ref signature) = cfg.extra().signature {
///         msg.push_str(&format!("Brought to you by {}\n", signature));
///     }
///     Ok(Response::new(Body::from(msg)))
/// }
///
/// fn main() {
///     Spirit::<Empty, Config>::new()
///         .config_defaults(DEFAULT_CONFIG)
///         .with(spirit_tokio::resources(
///             Config::listen,
///             spirit_hyper::server_configured(hello),
///             "server",
///         ))
///         .run(|spirit| {
/// #           let spirit = Arc::clone(spirit);
/// #           std::thread::spawn(move || spirit.terminate());
///             Ok(())
///         });
/// }
/// ```
pub fn server_configured<R, O, C, S, Fut, B>(
    service: S,
) -> impl ResourceConsumer<HyperServer<R>, O, C>
where
    C: Send + Sync + 'static,
    O: Send + Sync + 'static,
    R: ResourceConfig<O, C>,
    R::Resource: IntoIncoming,
    <R::Resource as IntoIncoming>::Connection: AsyncRead + AsyncWrite,
    S: Fn(&Arc<Spirit<O, C>>, &Arc<HyperServer<R>>, Request<Body>) -> Fut
        + Clone
        + Send
        + Sync
        + 'static,
    Fut: IntoFuture<Item = Response<B>> + Send + 'static,
    Fut::Future: Send + 'static,
    Fut::Error: Into<Box<Error + Send + Sync>>,
    B: Payload,
{
    let configure_service = move |spirit: &_, cfg: &_, _: &_, _: &_| {
        let service = service.clone();
        let spirit = Arc::clone(spirit);
        let cfg = Arc::clone(cfg);
        move || {
            let service = service.clone();
            let spirit = Arc::clone(&spirit);
            let cfg = Arc::clone(&cfg);
            hyper::service::service_fn(move |req| service(&spirit, &cfg, req))
        }
    };
    server(configure_service)
}

/// A [`ResourceConfig`] for hyper servers.
///
/// This is a wrapper around a `Transport` [`ResourceConfig`]. It takes something that accepts
/// connections ‒ like [`TcpListen`] and adds configuration specific for HTTP server.
///
/// This can then be paired with one of the [`ResourceConsumer`]s created by `server` functions to
/// spawn servers:
///
/// * [`server`]
/// * [`server_configured`]
/// * [`server_simple`]
/// * [`server_ok`]
///
/// See also the [`HttpServer`] type alias.
///
/// # TODO: Actually add the hyper-specific configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HyperServer<Transport> {
    #[serde(flatten)]
    transport: Transport,
}

impl<T> ExtraCfgCarrier for HyperServer<T>
where
    T: ExtraCfgCarrier,
{
    type Extra = T::Extra;
    fn extra(&self) -> &T::Extra {
        self.transport.extra()
    }
}

impl<O, C, T> ResourceConfig<O, C> for HyperServer<T>
where
    T: ResourceConfig<O, C>,
{
    type Seed = T::Seed;
    type Resource = T::Resource;
    fn create(&self, name: &str) -> Result<Self::Seed, FailError> {
        self.transport.create(name)
    }
    fn fork(&self, seed: &Self::Seed, name: &str) -> Result<Self::Resource, FailError> {
        self.transport.fork(seed, name)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.transport.scaled(name)
    }
    fn is_similar(&self, other: &Self, name: &str) -> bool {
        self.transport.is_similar(&other.transport, name)
    }
    fn install<N: Name>(builder: Builder<O, C>, name: &N) -> Builder<O, C> {
        T::install(builder, name)
    }
}

/// A type alias for http (plain TCP) hyper server.
pub type HttpServer<ExtraCfg = Empty> = HyperServer<TcpListen<ExtraCfg>>;
