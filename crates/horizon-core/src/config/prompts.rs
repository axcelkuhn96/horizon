//! Saved prompts: named texts the user inserts into a terminal panel from the command palette.
//!
//! They live in `config.yaml` twice: a global `prompts` list and one per workspace template.
//! Both are optional and left out of the file while empty, so configs written before this
//! feature load and re-serialise unchanged.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::WorkspaceConfig;
use crate::error::{Error, Result};

/// Longest prompt name, in characters.
pub const MAX_PROMPT_NAME_CHARS: usize = 40;
/// Longest prompt text, in bytes.
pub const MAX_PROMPT_TEXT_BYTES: usize = 8192;

/// A named text to paste into a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPrompt {
    pub name: String,
    pub text: String,
}

/// What is wrong with one prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptIssueKind {
    EmptyName,
    NameTooLong,
    NameHasControlChars,
    /// Another prompt of the same list has this name (case-insensitively).
    DuplicateName,
    EmptyText,
    TextTooLong,
    /// NUL, ESC or another control character other than newline and tab.
    TextHasControlChars,
}

impl PromptIssueKind {
    /// Short inline message for the settings editor.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::EmptyName => "Name is required",
            Self::NameTooLong | Self::TextTooLong => "Too long",
            Self::NameHasControlChars => "Name can't contain control characters",
            Self::DuplicateName => "Name already used",
            Self::EmptyText => "Text is required",
            Self::TextHasControlChars => "Text can't contain control characters",
        }
    }

    /// Whether the problem makes the prompt unsafe or unusable to keep (control characters,
    /// oversize, duplicate name), as opposed to an unfinished draft (empty name or text), which
    /// the editor only hints at.
    #[must_use]
    pub const fn is_hard(self) -> bool {
        !matches!(self, Self::EmptyName | Self::EmptyText)
    }

    /// Whether the problem is with the name field (else with the text field).
    #[must_use]
    pub const fn is_name_issue(self) -> bool {
        matches!(
            self,
            Self::EmptyName | Self::NameTooLong | Self::NameHasControlChars | Self::DuplicateName
        )
    }
}

/// One problem found in a prompt list; `index` points into that list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptIssue {
    pub index: usize,
    pub kind: PromptIssueKind,
}

/// Case-insensitive key two prompt names collide on.
fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Checks `list` and returns every problem, in list order. Newlines and tabs are fine in a
/// text; other control characters (NUL, ESC, ...) are not.
#[must_use]
pub fn validate_prompts(list: &[SavedPrompt]) -> Vec<PromptIssue> {
    let mut issues = Vec::new();
    let mut seen = HashSet::new();
    for (index, prompt) in list.iter().enumerate() {
        let name = prompt.name.trim();
        let mut flag = |kind| issues.push(PromptIssue { index, kind });
        if name.is_empty() {
            flag(PromptIssueKind::EmptyName);
        } else {
            if name.chars().count() > MAX_PROMPT_NAME_CHARS {
                flag(PromptIssueKind::NameTooLong);
            }
            if name.chars().any(char::is_control) {
                flag(PromptIssueKind::NameHasControlChars);
            }
            if !seen.insert(name_key(name)) {
                flag(PromptIssueKind::DuplicateName);
            }
        }
        if prompt.text.trim().is_empty() {
            flag(PromptIssueKind::EmptyText);
        } else {
            if prompt.text.len() > MAX_PROMPT_TEXT_BYTES {
                flag(PromptIssueKind::TextTooLong);
            }
            if prompt
                .text
                .chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
            {
                flag(PromptIssueKind::TextHasControlChars);
            }
        }
    }
    issues
}

/// Where an effective prompt comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptScope {
    Global,
    Workspace,
}

impl PromptScope {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }
}

/// A prompt as the user sees it for one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectivePrompt {
    pub name: String,
    pub text: String,
    pub scope: PromptScope,
}

/// Whether a prompt is complete enough to offer (draft rows with an empty name or text are not).
fn is_usable(prompt: &SavedPrompt) -> bool {
    !prompt.name.trim().is_empty() && !prompt.text.trim().is_empty()
}

/// Why `prompt` must not be offered, when it is unsafe or unusable to keep: a hard problem
/// (control characters, oversize) or a name an earlier usable prompt of the same list already
/// has. Unfinished drafts are skipped too but report nothing here (they are not an error).
/// `seen` collects the names of the usable prompts so far.
fn skip_reason(prompt: &SavedPrompt, seen: &mut HashSet<String>) -> Option<PromptIssueKind> {
    let hard = validate_prompts(std::slice::from_ref(prompt))
        .into_iter()
        .map(|issue| issue.kind)
        .find(|kind| kind.is_hard());
    hard.or_else(|| {
        (is_usable(prompt) && !seen.insert(name_key(&prompt.name))).then_some(PromptIssueKind::DuplicateName)
    })
}

/// The prompts of `list` that can be offered: complete, safe and, for a repeated name, the first.
fn usable_prompts(list: &[SavedPrompt]) -> Vec<&SavedPrompt> {
    let mut seen = HashSet::new();
    list.iter()
        .filter(|prompt| skip_reason(prompt, &mut seen).is_none() && is_usable(prompt))
        .collect()
}

/// The prompts of `list` that [`effective_prompts`] leaves out because they are unsafe or
/// repeat a name (unfinished drafts are not listed). They stay in the config as written; this
/// is for logging them.
#[must_use]
pub fn skipped_prompts(list: &[SavedPrompt]) -> Vec<PromptIssue> {
    let mut seen = HashSet::new();
    list.iter()
        .enumerate()
        .filter_map(|(index, prompt)| skip_reason(prompt, &mut seen).map(|kind| PromptIssue { index, kind }))
        .collect()
}

/// The prompts available in a workspace: its own first, then the global ones it does not
/// shadow. A workspace prompt hides the global prompt of the same name (case-insensitively).
/// Unfinished drafts (empty name or text) are skipped, so inserting one can never send a bare
/// Enter; so are prompts with control characters or oversize text and later repeats of a name
/// within one list. Those stay in the config as written (see [`skipped_prompts`]).
#[must_use]
pub fn effective_prompts(global: &[SavedPrompt], workspace: &[SavedPrompt]) -> Vec<EffectivePrompt> {
    let own = usable_prompts(workspace);
    let shadowed: HashSet<String> = own.iter().map(|prompt| name_key(&prompt.name)).collect();
    let inherited = usable_prompts(global)
        .into_iter()
        .filter(|prompt| !shadowed.contains(&name_key(&prompt.name)));
    own.into_iter()
        .map(|prompt| (prompt, PromptScope::Workspace))
        .chain(inherited.map(|prompt| (prompt, PromptScope::Global)))
        .map(|(prompt, scope)| EffectivePrompt {
            name: prompt.name.trim().to_owned(),
            text: prompt.text.clone(),
            scope,
        })
        .collect()
}

/// Validates the global list and every workspace template's list. Only hard problems fail;
/// unfinished drafts (empty name or text) are allowed so a half-written row can be saved.
///
/// # Errors
///
/// Returns [`Error::Config`] naming the first offending prompt.
pub fn validate_all_prompts(global: &[SavedPrompt], workspaces: &[WorkspaceConfig]) -> Result<()> {
    let describe = |scope: &str, list: &[SavedPrompt]| -> Result<()> {
        match validate_prompts(list).iter().find(|issue| issue.kind.is_hard()) {
            None => Ok(()),
            Some(issue) => Err(Error::Config(format!(
                "{scope} prompt #{}: {}",
                issue.index + 1,
                issue.kind.message()
            ))),
        }
    };
    describe("prompts:", global)?;
    for workspace in workspaces {
        describe(&format!("workspace '{}':", workspace.name), &workspace.prompts)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(name: &str, text: &str) -> SavedPrompt {
        SavedPrompt {
            name: name.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_plain_list_has_no_issues() {
        let list = [prompt("review", "Review this diff.\nBe terse.\n\tIndented")];

        assert!(validate_prompts(&list).is_empty());
    }

    #[test]
    fn empty_name_and_text_are_reported_per_row() {
        let list = [prompt("  ", "x"), prompt("ok", "")];

        let kinds: Vec<_> = validate_prompts(&list).iter().map(|i| (i.index, i.kind)).collect();

        assert_eq!(
            kinds,
            [(0, PromptIssueKind::EmptyName), (1, PromptIssueKind::EmptyText)]
        );
    }

    #[test]
    fn duplicate_names_ignore_case_and_flag_the_later_row() {
        let list = [prompt("Review", "a"), prompt("review ", "b"), prompt("other", "c")];

        let issues = validate_prompts(&list);

        assert_eq!(
            issues,
            [PromptIssue {
                index: 1,
                kind: PromptIssueKind::DuplicateName
            }]
        );
    }

    #[test]
    fn name_length_counts_characters_not_bytes() {
        let ok = "\u{e9}".repeat(MAX_PROMPT_NAME_CHARS);
        let long = "a".repeat(MAX_PROMPT_NAME_CHARS + 1);

        assert!(validate_prompts(&[prompt(&ok, "x")]).is_empty());
        assert_eq!(
            validate_prompts(&[prompt(&long, "x")])[0].kind,
            PromptIssueKind::NameTooLong
        );
    }

    #[test]
    fn text_over_the_byte_cap_is_too_long() {
        let at_cap = "a".repeat(MAX_PROMPT_TEXT_BYTES);
        let over = "a".repeat(MAX_PROMPT_TEXT_BYTES + 1);

        assert!(validate_prompts(&[prompt("n", &at_cap)]).is_empty());
        assert_eq!(
            validate_prompts(&[prompt("n", &over)])[0].kind,
            PromptIssueKind::TextTooLong
        );
    }

    #[test]
    fn control_characters_are_rejected_except_newline_and_tab() {
        for bad in ["a\u{0}b", "a\u{1b}[31mb", "a\u{7}b", "a\rb"] {
            let issues = validate_prompts(&[prompt("n", bad)]);
            assert_eq!(issues[0].kind, PromptIssueKind::TextHasControlChars, "{bad:?}");
        }
        assert_eq!(
            validate_prompts(&[prompt("a\nb", "x")])[0].kind,
            PromptIssueKind::NameHasControlChars
        );
    }

    #[test]
    fn workspace_prompts_come_first_and_shadow_the_global_one() {
        let global = [prompt("review", "global"), prompt("tests", "write tests")];
        let workspace = [prompt("Review", "local"), prompt("deploy", "ship it")];

        let list = effective_prompts(&global, &workspace);

        let view: Vec<_> = list
            .iter()
            .map(|p| (p.name.as_str(), p.text.as_str(), p.scope))
            .collect();
        assert_eq!(
            view,
            [
                ("Review", "local", PromptScope::Workspace),
                ("deploy", "ship it", PromptScope::Workspace),
                ("tests", "write tests", PromptScope::Global),
            ]
        );
    }

    #[test]
    fn without_workspace_prompts_the_global_list_is_kept_in_order() {
        let global = [prompt("b", "1"), prompt("a", "2")];

        let list = effective_prompts(&global, &[]);

        assert_eq!(list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert!(list.iter().all(|p| p.scope == PromptScope::Global));
    }

    #[test]
    fn empty_name_or_text_is_a_draft_not_a_hard_error() {
        assert!(!PromptIssueKind::EmptyName.is_hard());
        assert!(!PromptIssueKind::EmptyText.is_hard());
        assert!(PromptIssueKind::DuplicateName.is_hard());
        assert!(PromptIssueKind::TextTooLong.is_hard());
        assert!(PromptIssueKind::TextHasControlChars.is_hard());
        assert!(validate_all_prompts(&[prompt("", ""), prompt("a", "")], &[]).is_ok());
        assert!(validate_all_prompts(&[prompt("a", "x"), prompt("A", "y")], &[]).is_err());
    }

    #[test]
    fn effective_prompts_skip_drafts_and_they_do_not_shadow() {
        let list = effective_prompts(&[prompt("a", "global")], &[prompt("a", ""), prompt("", "x")]);

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].scope, PromptScope::Global);
    }

    #[test]
    fn effective_prompts_skip_unsafe_ones_and_repeated_names() {
        let list = [
            prompt("a", "one"),
            prompt("A", "dup"),
            prompt("b", "x\u{1b}y"),
            prompt("c", &"z".repeat(MAX_PROMPT_TEXT_BYTES + 1)),
            prompt("draft", ""),
            prompt("d", "fine"),
        ];

        let names: Vec<_> = effective_prompts(&list, &[]).into_iter().map(|p| p.name).collect();

        assert_eq!(names, ["a", "d"]);
        let skipped: Vec<_> = skipped_prompts(&list).iter().map(|i| (i.index, i.kind)).collect();
        assert_eq!(
            skipped,
            [
                (1, PromptIssueKind::DuplicateName),
                (2, PromptIssueKind::TextHasControlChars),
                (3, PromptIssueKind::TextTooLong),
            ]
        );
    }

    #[test]
    fn a_draft_does_not_make_a_later_prompt_a_duplicate() {
        let list = [prompt("a", ""), prompt("a", "real")];

        let names: Vec<_> = effective_prompts(&list, &[]).into_iter().map(|p| p.name).collect();

        assert_eq!(names, ["a"]);
        assert!(skipped_prompts(&list).is_empty());
    }
}
