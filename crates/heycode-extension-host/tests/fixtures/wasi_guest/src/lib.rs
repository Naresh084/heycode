#[cfg(not(feature = "scoped"))]
wit_bindgen::generate!({
    path: "wit",
    world: "code-plugin",
});

#[cfg(feature = "scoped")]
wit_bindgen::generate!({
    path: "wit",
    world: "code-plugin-filesystem-network",
    generate_all,
});

use exports::heycode::code_plugin::plugin::{
    Guest, InitializeRequest, InvocationError, InvocationRequest, ProtocolError, Ready,
};

struct Fixture;

impl Guest for Fixture {
    fn initialize(request: InitializeRequest) -> Result<Ready, ProtocolError> {
        #[cfg(feature = "scoped")]
        {
            let directories = wasi::filesystem::preopens::get_directories();
            drop(std::hint::black_box(directories));
        }
        Ok(Ready {
            protocol_version: request.protocol_version,
            session_id: request.session_id,
            package_id: request.package_id,
            package_version: request.package_version,
            package_digest: request.package_digest,
            component_digest: request.component_digest,
            accepted_capabilities: request.granted_capabilities,
            contributions: request.contributions,
        })
    }

    fn invoke(request: InvocationRequest) -> Result<Vec<u8>, InvocationError> {
        if request.operation == "spin" {
            loop {
                std::hint::black_box(());
            }
        }
        Ok(request.input_json)
    }
}

export!(Fixture);

#[cfg(feature = "scoped")]
#[used]
static RETAIN_NETWORK_IMPORT: fn() = retain_network_import;

#[cfg(feature = "scoped")]
fn retain_network_import() {
    use core::future::Future as _;
    use core::task::{Context, Poll, Waker};
    use std::sync::Arc;
    use std::task::Wake;

    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    let mut lookup = Box::pin(wasi::sockets::ip_name_lookup::resolve_addresses(
        "127.0.0.1".to_owned(),
    ));
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    let _: Poll<_> = lookup.as_mut().poll(&mut context);
}
