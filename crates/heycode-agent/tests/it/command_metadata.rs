//! CMD01 command metadata/availability catalog contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandAvailability, CommandDescriptor, CommandRegistry,
    CommandSource, CommandTiming,
};

struct Described {
    descriptor: CommandDescriptor,
    availability: CommandAvailability,
}

#[async_trait]
impl Command for Described {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        self.availability.clone()
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, _args: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[test]
fn descriptors_validate_structured_args_and_catalog_retains_all_metadata() {
    let descriptor = CommandDescriptor::new(
        "deploy",
        "Deploy the selected target",
        vec![
            CommandArgument::required("target", "Target environment").unwrap(),
            CommandArgument::optional("note", "Optional release note")
                .unwrap()
                .variadic(),
        ],
        CommandTiming::Interrupting,
        CommandSource::from_plugin("test-plugin").unwrap(),
    )
    .unwrap()
    .with_shortcut("Ctrl+D")
    .unwrap();
    assert_eq!(descriptor.synopsis(), "/deploy <target> [note...]");

    let mut registry = CommandRegistry::new();
    registry
        .register(std::sync::Arc::new(Described {
            descriptor: descriptor.clone(),
            availability: CommandAvailability::unavailable("Connect a deployment runtime").unwrap(),
        }))
        .unwrap();
    let catalog = registry.catalog().unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].descriptor, descriptor);
    assert_eq!(catalog[0].descriptor.timing(), CommandTiming::Interrupting);
    assert_eq!(catalog[0].descriptor.source().plugin(), "test-plugin");
    assert_eq!(catalog[0].descriptor.shortcut(), Some("Ctrl+D"));
    assert_eq!(
        catalog[0].availability.reason(),
        Some("Connect a deployment runtime")
    );
    assert!(!catalog[0].availability.is_available());
    assert_eq!(
        registry.help_lines().unwrap(),
        [
            "/deploy <target> [note...] — Deploy the selected target [unavailable: Connect a deployment runtime]"
        ]
    );
}

#[test]
fn central_aliases_dispatch_without_duplicate_inventory_rows_or_hidden_collisions() {
    let source = CommandSource::from_plugin("test-plugin").unwrap();
    let aliases = [
        ("new", &["clear", "reset"][..]),
        ("quit", &["exit"][..]),
        ("usage", &["cost"][..]),
        ("keymap", &["keybindings"][..]),
        ("list-agents", &["peers"][..]),
        ("connect", &["login"][..]),
        ("permissions", &["allowed-tools"][..]),
        ("plugins", &["plugin"][..]),
        ("resume", &["continue"][..]),
        ("rename", &["name"][..]),
        ("rewind", &["checkpoint", "undo"][..]),
        ("schedule", &["routines"][..]),
        ("background", &["bg"][..]),
        ("tasks", &["bashes"][..]),
    ];
    let mut registry = CommandRegistry::new();
    for (canonical, expected_aliases) in aliases {
        let descriptor = CommandDescriptor::new(
            canonical,
            "test command",
            Vec::new(),
            CommandTiming::Immediate,
            source.clone(),
        )
        .unwrap();
        assert_eq!(descriptor.aliases(), expected_aliases);
        registry
            .register(std::sync::Arc::new(Described {
                descriptor,
                availability: CommandAvailability::available(),
            }))
            .unwrap();
        for alias in expected_aliases {
            assert_eq!(
                registry.get(alias).unwrap().unwrap().descriptor().id(),
                canonical
            );
        }
    }
    assert_eq!(registry.names().unwrap().len(), aliases.len());
    assert!(registry.help_lines().unwrap()[0].contains("aliases: /clear, /reset"));

    let conflict = CommandDescriptor::new(
        "clear",
        "conflicts with the central alias",
        Vec::new(),
        CommandTiming::Immediate,
        source.clone(),
    )
    .unwrap();
    assert!(matches!(
        registry.register(std::sync::Arc::new(Described {
            descriptor: conflict,
            availability: CommandAvailability::available(),
        })),
        Err(heycode_agent::CommandRegistryError::Duplicate { id }) if id == "clear"
    ));

    let mut reverse = CommandRegistry::new();
    reverse
        .register(std::sync::Arc::new(Described {
            descriptor: CommandDescriptor::new(
                "clear",
                "claims the future alias first",
                Vec::new(),
                CommandTiming::Immediate,
                source,
            )
            .unwrap(),
            availability: CommandAvailability::available(),
        }))
        .unwrap();
    let new = CommandDescriptor::new(
        "new",
        "must fail atomically",
        Vec::new(),
        CommandTiming::Immediate,
        CommandSource::from_plugin("test-plugin").unwrap(),
    )
    .unwrap();
    assert!(matches!(
        reverse.register(std::sync::Arc::new(Described {
            descriptor: new,
            availability: CommandAvailability::available(),
        })),
        Err(heycode_agent::CommandRegistryError::Duplicate { id }) if id == "clear"
    ));
    assert!(reverse.get("new").unwrap().is_none());
}

#[test]
fn invalid_metadata_and_duplicate_ids_fail_before_catalog_publication() {
    assert!(CommandSource::from_plugin("Bad_Plugin").is_err());
    assert!(CommandArgument::required("Bad_Arg", "good").is_err());
    assert!(CommandAvailability::unavailable(" bad ").is_err());
    assert!(
        CommandDescriptor::new(
            "bad id",
            "description",
            Vec::new(),
            CommandTiming::Immediate,
            CommandSource::from_plugin("test-plugin").unwrap(),
        )
        .is_err()
    );
    assert!(
        CommandDescriptor::new(
            "bad-order",
            "description",
            vec![
                CommandArgument::optional("first", "optional").unwrap(),
                CommandArgument::required("second", "required").unwrap(),
            ],
            CommandTiming::Immediate,
            CommandSource::from_plugin("test-plugin").unwrap(),
        )
        .is_err()
    );

    let descriptor = CommandDescriptor::new(
        "same",
        "same",
        Vec::new(),
        CommandTiming::Immediate,
        CommandSource::from_plugin("test-plugin").unwrap(),
    )
    .unwrap();
    let mut registry = CommandRegistry::new();
    for expected_ok in [true, false] {
        let result = registry.register(std::sync::Arc::new(Described {
            descriptor: descriptor.clone(),
            availability: CommandAvailability::available(),
        }));
        assert_eq!(result.is_ok(), expected_ok);
    }
    assert_eq!(registry.catalog().unwrap().len(), 1);
}
