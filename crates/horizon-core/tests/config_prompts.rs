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
fn validate_rejects_empty_name_and_duplicate_case_insensitive() {
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
fn a_bad_workspace_prompt_fails_the_whole_config() {
    let config = Config {
        workspaces: vec![workspace("Alpha", vec![prompt("", "x")])],
        ..Config::default()
    };

    assert!(config.validate().is_err());
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
