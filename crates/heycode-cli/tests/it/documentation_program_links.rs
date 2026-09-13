//! DOC01 authoritative engineering program link freshness.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn every_root_status_tracker_and_feature_doc_links_master_plan_and_active_tasks() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap();
    let documents = [
        (
            "CONTRIBUTING.md",
            ["docs/engineering/README.md", "docs/engineering/TASKS.md"],
        ),
        (
            "docs/STATUS.md",
            ["engineering/README.md", "engineering/TASKS.md"],
        ),
        (
            "docs/TASKS.md",
            ["engineering/README.md", "engineering/TASKS.md"],
        ),
        (
            "FEATURES.md",
            ["docs/engineering/README.md", "docs/engineering/TASKS.md"],
        ),
    ];

    for (document, targets) in documents {
        let path = repo.join(document);
        let text = std::fs::read_to_string(&path).unwrap();
        for target in targets {
            assert!(
                text.contains(&format!("]({target})")),
                "{document} must directly link `{target}`"
            );
            let resolved = path.parent().unwrap().join(target);
            assert!(
                resolved.is_file(),
                "{document} link target does not resolve: {}",
                resolved.display()
            );
        }
    }
}
