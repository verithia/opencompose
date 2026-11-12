use super::*;

fn recorded_edits(count: usize) -> (UndoStack, Project) {
    let mut stack = UndoStack::new();
    let mut project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
    for index in 0..count {
        let before = project.clone();
        project.properties.tempo = 121.0 + index as f64;
        stack
            .push(
                before,
                &project,
                UndoEdit::new(format!("tempo {index}"), UndoScope::Transport),
            )
            .unwrap();
    }
    (stack, project)
}

fn assert_entries_unchanged(actual: &[UndoEntry], expected: &[UndoEntry]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.forward, expected.forward);
        assert_eq!(actual.inverse, expected.inverse);
        assert_eq!(actual.label, expected.label);
        assert_eq!(actual.scopes, expected.scopes);
        assert_eq!(actual.timestamp, expected.timestamp);
    }
}

#[test]
fn corrupt_redo_keeps_document_and_both_history_stacks() {
    let (mut stack, mut project) = recorded_edits(2);
    stack.undo(&mut project).unwrap();
    stack.redo.last_mut().unwrap().inverse = b"corrupt redo".to_vec();
    let before = project.clone();
    let undo = stack.undo.clone();
    let redo = stack.redo.clone();
    assert!(
        stack
            .redo(&mut project)
            .unwrap_err()
            .contains("could not redo")
    );
    assert_eq!(project, before);
    assert_entries_unchanged(&stack.undo, &undo);
    assert_entries_unchanged(&stack.redo, &redo);
}

#[test]
fn corrupt_later_patch_makes_a_backward_history_jump_atomic() {
    let (mut stack, mut project) = recorded_edits(3);
    // The first decoded step is valid; failure occurs deeper in the jump.
    stack.undo[0].inverse = b"corrupt inverse".to_vec();
    let before = project.clone();
    let undo = stack.undo.clone();
    let redo = stack.redo.clone();
    assert!(
        stack
            .restore_undo_level(0, &mut project)
            .unwrap_err()
            .contains("could not restore")
    );
    assert_eq!(project, before);
    assert_entries_unchanged(&stack.undo, &undo);
    assert_entries_unchanged(&stack.redo, &redo);
}

#[test]
fn corrupt_later_patch_makes_a_forward_history_jump_atomic() {
    let (mut stack, mut project) = recorded_edits(3);
    stack.restore_undo_level(0, &mut project).unwrap();
    stack.redo[0].inverse = b"corrupt forward".to_vec();
    let before = project.clone();
    let undo = stack.undo.clone();
    let redo = stack.redo.clone();
    assert!(stack.restore_undo_level(3, &mut project).is_err());
    assert_eq!(project, before);
    assert_entries_unchanged(&stack.undo, &undo);
    assert_entries_unchanged(&stack.redo, &redo);
}

#[test]
fn an_empty_redo_is_an_error_without_changing_the_document() {
    let (mut stack, mut project) = recorded_edits(0);
    let before = project.clone();
    assert_eq!(stack.redo(&mut project).unwrap_err(), "Nothing to redo");
    assert_eq!(project, before);
}
