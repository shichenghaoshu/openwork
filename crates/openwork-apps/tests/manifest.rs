use openwork_apps::{AppId, AppRegistry, CapabilityPack};
use std::path::PathBuf;

#[test]
fn checked_in_app_registry_is_valid_and_complete() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry/apps");
    let registry = AppRegistry::load(&directory).expect("checked-in app registry");
    assert_eq!(registry.list().len(), 4);
    for id in [
        "org.openwork.github",
        "org.openwork.feishu",
        "org.openwork.libreoffice",
        "org.openwork.erp.procurement",
    ] {
        assert!(
            registry.get(&AppId::parse(id).expect("app id")).is_some(),
            "missing {id}"
        );
    }

    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../contracts/schemas/capability-pack.v1.schema.json"
    ))
    .expect("capability schema JSON");
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");
    for manifest in std::fs::read_dir(directory).expect("registry directory") {
        let path = manifest.expect("registry entry").path();
        let input = std::fs::read_to_string(&path).expect("manifest");
        let value: serde_json::Value = yaml_serde::from_str(&input).expect("manifest YAML");
        assert!(
            validator.is_valid(&value),
            "schema rejected {}",
            path.display()
        );
    }
}

#[test]
fn manifest_rejects_unknown_fields_and_duplicate_capabilities() {
    let unknown = include_str!("../../../registry/apps/github.yaml").replace(
        "  description: Read repositories",
        "  unexpected: true\n  description: Read repositories",
    );
    assert!(CapabilityPack::from_yaml(&unknown).is_err());

    let duplicate = include_str!("../../../registry/apps/feishu.yaml")
        .replace("  - id: feishu.message.read", "  - id: feishu.chat.search");
    assert!(CapabilityPack::from_yaml(&duplicate).is_err());
}
