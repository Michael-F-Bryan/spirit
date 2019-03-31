use failure::{ensure, Error};
use serde::Deserialize;
use spirit::extension::{Extensible, Extension};
use spirit::fragment::driver::Trivial;
use spirit::fragment::Fragment;
use spirit::fragment::Installer;
use spirit::prelude::*;

#[derive(Clone, Debug, Default, Deserialize)]
struct WithExt;

impl WithExt {
    fn extension<E, F>(_extract: F) -> impl Extension<E>
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
    fn install(&mut self, _: R, _: &str) {}
}

#[derive(Clone, Debug, Default, Deserialize)]
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

fn check_answer(answer: &usize) -> Result<(), Error> {
    ensure!(*answer <= 42, "The answer is too big");
    Ok(())
}

// TODO: Other traits here!
#[derive(Clone, Debug, Default, Spirit, Deserialize)]
struct Cfg {
    unused: String,

    #[spirit(immutable)]
    imut: i32,

    #[spirit(extension)]
    with_ext: WithExt,

    #[spirit(pipeline(install = "NopInstaller", check))]
    pipelined: Pipelined,

    #[spirit(validate = "check_answer")]
    answer: usize,
}

fn main() {
    Spirit::<Empty, Cfg>::new()
        .with(Cfg::extension)
        .run(|_s| Ok(()));
}
