use nsh_core::prelude::*;

#[derive(Clone)]
pub(crate) struct ActionOwner(Owner);

impl ActionOwner {
    pub(crate) fn install() {
        // Capture FalconApp's root before transient popup and Select owners are created.
        provide_context(Self(Owner::current()));
    }

    pub(crate) fn current() -> Self {
        use_context::<Self>().expect("ActionOwner must be installed by FalconApp")
    }

    pub(crate) fn run<T>(&self, callback: impl FnOnce() -> T) -> T {
        self.0.clone().run(callback)
    }
}
