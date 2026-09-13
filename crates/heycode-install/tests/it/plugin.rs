//! Q14 product reachability through an effect-owned service plugin.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt as _;

    use heycode_core::{Plugin, PluginContributionKind, PluginDescriptor};
    use heycode_install::{
        GhReleaseManagerConfig, ReleaseManager, ReleaseManagerError, SERVICE_RELEASE_MANAGER,
        gh_release_manager_plugin,
    };

    use crate::it::support::{platform, trust};

    struct SubprocessFixture;

    impl Plugin for SubprocessFixture {
        fn name(&self) -> &'static str {
            "subprocess-fixture"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(self.name(), "1", &[PluginContributionKind::Service])
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_exec::SERVICE_SUBPROCESS]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            context.provide(
                heycode_exec::SERVICE_SUBPROCESS,
                self.name(),
                heycode_exec::SubprocessService::local(),
            )
        }
    }

    #[test]
    fn plugin_publishes_manager_and_shutdown_closes_held_handle() {
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("gh-fixture");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = GhReleaseManagerConfig::new(
            temp.path().join("install"),
            temp.path().join("scratch"),
            program.into_os_string(),
            platform(),
            trust(),
        );
        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(SubprocessFixture),
            gh_release_manager_plugin(config),
        ];
        let mut context = heycode_core::compose(&plugins).unwrap();
        let manager = context
            .get::<ReleaseManager>(SERVICE_RELEASE_MANAGER)
            .expect("release manager service");
        assert_eq!(
            context.owner_of(SERVICE_RELEASE_MANAGER),
            Some("release-manager-gh")
        );
        assert!(manager.state().is_ok());

        context.shutdown();

        assert_eq!(manager.state().unwrap_err(), ReleaseManagerError::Closed);
    }
}
