use systemg::config::parse_config_manifest;

#[test]
fn repo_yaml_examples_parse() {
    let root = env!("CARGO_MANIFEST_DIR");
    for path in [
        "config.yaml",
        "examples/hello-world/hello-world.sysg.yaml",
        "examples/crud/crud.sysg.yaml",
        "examples/orchestrator/systemg.yaml",
    ] {
        let content = std::fs::read_to_string(format!("{root}/{path}")).unwrap();
        parse_config_manifest(&content)
            .unwrap_or_else(|e| panic!("{path} failed to parse: {e}"));
    }
}
