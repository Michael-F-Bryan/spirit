use failure::Error;
use spirit::Spirit;
use spirit::extension::{Extension, Extensible};
use spirit::fragment::Fragment;
use spirit::fragment::driver::Trivial;
use spirit::fragment::Installer;

struct WithExt;

impl WithExt {
    fn extension<E, F>(extract: F) -> impl Extension<E>
    where
        E: Extensible<Ok = E>,
        F: FnOnce(&E::Config) -> &Self + Send + 'static,
    {
        |ext: E| ext
    }
}

struct NopInstaller;

impl<R, O, C> Installer<R, O, C> for NopInstaller {
    type UninstallHandle = ();
    fn install(&mut self, _: R, _: &str) { }
}

struct Pipelined;

impl Fragment for Pipelined {
    type Driver = Trivial;
    type Installer = ();
    type Seed = ();
    type Resource = ();
    fn make_seed(&self, _: &str) -> Result<(), Error> {
        Ok(())
    }
    fn make_resource(&self, _: &mut (), _: &str) -> Result<(), Error> {
        Ok(())
    }
}

#[derive(Spirit)]
struct Cfg {
    #[spirit(immutable)]
    unused: String,

    #[spirit(extension)]
    with_ext: WithExt,

    // TODO: Why doesn't this want to implement the Extension? Isn't it a nice happy pipeline?
    #[spirit(pipeline(install = "NopInstaller", check))]
    pipelined: Pipelined,
}

fn main() {

}
