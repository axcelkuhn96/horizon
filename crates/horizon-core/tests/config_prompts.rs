use horizon_core::{Config, PromptScope, SavedPrompt, WorkspaceConfig, effective_prompts, validate_prompts};

fn prompt(name: &str, text: &str) -> SavedPrompt {
    SavedPrompt {
        name: name.to_owned(),
        text: text.to_owned(),
    }
}

fn workspace(name: &str, prompts: Vec<SavedPrompt>) -> WorkspaceConfig {
    WorkspaceConfig {
        name: name.to_owned(),
        color: None,
        cwd: None,
        position: None,
        terminals: Vec::new(),
        sidebar_collapsed: false,
        prompts,
    }
}

#[test]
fn prompts_default_empty_and_omitted_from_yaml() {
    let config = Config::default();
    assert!(config.prompts.is_empty());

    let yaml = config.to_yaml().expect("serialize");

    assert!(!yaml.contains("prompts"), "no noise key for an empty list:\n{yaml}");
}

#[test]
fn old_yaml_round_trips_without_prompts_key() {
    let old = "workspaces:\n  - name: Alpha\n    terminals: []\n";

    let config: Config = serde_yaml::from_str(old).expect("old yaml parses");
    let yaml = config.to_yaml().expect("serialize");

    assert!(config.prompts.is_empty());
    assert!(config.workspaces[0].prompts.is_empty());
    assert!(!yaml.contains("prompts"), "{yaml}");
}

#[test]
fn global_and_workspace_prompts_round_trip() {
    let config = Config {
        prompts: vec![prompt("review", "Review this diff.\nBe terse.")],
        workspaces: vec![workspace("Alpha", vec![prompt("deploy", "Ship it.")])],
        ..Config::default()
    };

    let yaml = config.to_yaml().expect("serialize");
    let reloaded = Config::from_yaml(&yaml).expect("reload validates");

    assert_eq!(reloaded.prompts, config.prompts);
    assert_eq!(reloaded.workspaces[0].prompts, config.workspaces[0].prompts);
}

#[test]
fn validate_reports_empty_name_but_only_rejects_duplicates() {
    assert!(!validate_prompts(&[prompt("", "x")]).is_empty());
    assert!(!validate_prompts(&[prompt("Review", "x"), prompt("review", "y")]).is_empty());

    let config = Config {
        prompts: vec![prompt("Review", "x"), prompt("review", "y")],
        ..Config::default()
    };
    assert!(config.validate().is_err());
}

#[test]
fn validate_rejects_oversize_text_and_control_chars() {
    assert!(!validate_prompts(&[prompt("n", &"a".repeat(8193))]).is_empty());
    assert!(!validate_prompts(&[prompt("n", "a\u{0}b")]).is_empty());
    assert!(!validate_prompts(&[prompt("n", "a\u{1b}b")]).is_empty());
    assert!(validate_prompts(&[prompt("n", "a\nb")]).is_empty());
}

#[test]
fn a_bad_workspace_prompt_fails_validation() {
    let config = Config {
        workspaces: vec![workspace("Alpha", vec![prompt("n", "a\u{0}b")])],
        ..Config::default()
    };

    assert!(config.validate().is_err());
}

#[test]
fn unfinished_drafts_pass_validation_and_round_trip() {
    let config = Config {
        prompts: vec![prompt("", ""), prompt("draft", "")],
        workspaces: vec![workspace("Alpha", vec![prompt("", "text only")])],
        ..Config::default()
    };

    config.validate().expect("drafts are not fatal");
    let reloaded = Config::from_yaml(&config.to_yaml().expect("serialize")).expect("reload");

    assert_eq!(reloaded.prompts, config.prompts);
    assert_eq!(reloaded.workspaces[0].prompts, config.workspaces[0].prompts);
}

#[test]
fn effective_skips_unfinished_drafts() {
    let list = effective_prompts(
        &[prompt("g", ""), prompt("", "x"), prompt("ok", "go")],
        &[prompt("w", "  "), prompt("g", "")],
    );

    let names: Vec<_> = list.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["ok"]);
}

fn load_yaml(yaml: &str) -> Config {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, yaml).expect("write config");
    Config::load(Some(&path)).expect("a bad prompt must not make the config unloadable")
}

#[test]
fn load_drops_invalid_prompts_instead_of_failing() {
    let long = "a".repeat(8193);
    let yaml = format!(
        "prompts:\n  - name: ok\n    text: fine\n  - name: OK\n    text: duplicate\n  - name: big\n    text: {long}\n  - name: esc\n    text: \"a\\u001bb\"\n  - name: crlf\n    text: \"a\\r\\nb\"\n  - name: draft\n    text: \"\"\nworkspaces:\n  - name: Alpha\n    terminals: []\n    prompts:\n      - name: w\n        text: \"x\\u0000y\"\n      - name: keep\n        text: keep this\n"
    );

    let config = load_yaml(&yaml);

    let names: Vec<_> = config.prompts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["ok", "draft"]);
    assert_eq!(config.prompts[0].text, "fine");
    let workspace: Vec<_> = config.workspaces[0].prompts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(workspace, ["keep"]);
}

#[test]
fn load_keeps_valid_prompts_untouched() {
    let config = load_yaml("prompts:\n  - name: a\n    text: \"l1\\nl2\"\n");

    assert_eq!(config.prompts, [prompt("a", "l1\nl2")]);
}

#[test]
fn effective_workspace_overrides_global_by_name() {
    let list = effective_prompts(&[prompt("review", "global")], &[prompt("REVIEW", "local")]);

    assert_eq!(list.len(), 1);
    assert_eq!(list[0].text, "local");
    assert_eq!(list[0].scope, PromptScope::Workspace);
}

#[test]
fn effective_orders_workspace_first() {
    let list = effective_prompts(&[prompt("g1", "a"), prompt("g2", "b")], &[prompt("w1", "c")]);

    let names: Vec<_> = list.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["w1", "g1", "g2"]);
}
