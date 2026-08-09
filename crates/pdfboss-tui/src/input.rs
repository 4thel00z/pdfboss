//! Key-event → intent mapping. Pure so bindings are unit-testable.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

/// Every intent a key press can express.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    MoveUp,
    MoveDown,
    Collapse,
    Expand,
    FocusNext,
    Activate,
    Back,
    Top,
    Bottom,
    PageUp,
    PageDown,
    CycleView,
    TogglePreview,
    ToggleMarkdown,
    OpenSearch,
    SearchChar(char),
    SearchBackspace,
    SearchAccept,
    SearchCancel,
    NextHit,
    PrevHit,
    Quit,
    Noop,
}

/// Maps a key event. `search_input` reroutes printable keys into the
/// status-bar query (Esc cancels the search instead of quitting).
pub fn action_for(key: KeyEvent, search_input: bool) -> Action {
    if key.kind != KeyEventKind::Press {
        return Action::Noop;
    }
    if search_input {
        return match key.code {
            KeyCode::Esc => Action::SearchCancel,
            KeyCode::Enter => Action::SearchAccept,
            KeyCode::Backspace => Action::SearchBackspace,
            KeyCode::Char(c) => Action::SearchChar(c),
            _ => Action::Noop,
        };
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Char('/') => Action::OpenSearch,
        KeyCode::Char('n') => Action::NextHit,
        KeyCode::Char('N') => Action::PrevHit,
        KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
        KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
        KeyCode::Left | KeyCode::Char('h') => Action::Collapse,
        KeyCode::Right | KeyCode::Char('l') => Action::Expand,
        KeyCode::Tab => Action::FocusNext,
        KeyCode::Enter => Action::Activate,
        KeyCode::Backspace => Action::Back,
        KeyCode::Char('g') => Action::Top,
        KeyCode::Char('G') => Action::Bottom,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Char('d') => Action::CycleView,
        KeyCode::Char('p') => Action::TogglePreview,
        KeyCode::Char('m') => Action::ToggleMarkdown,
        _ => Action::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn normal_mode_bindings() {
        assert_eq!(action_for(press(KeyCode::Char('q')), false), Action::Quit);
        assert_eq!(action_for(press(KeyCode::Esc), false), Action::Quit);
        assert_eq!(
            action_for(press(KeyCode::Char('/')), false),
            Action::OpenSearch
        );
        assert_eq!(
            action_for(press(KeyCode::Char('j')), false),
            Action::MoveDown
        );
        assert_eq!(action_for(press(KeyCode::Down), false), Action::MoveDown);
        assert_eq!(action_for(press(KeyCode::Char('k')), false), Action::MoveUp);
        assert_eq!(action_for(press(KeyCode::Up), false), Action::MoveUp);
        assert_eq!(
            action_for(press(KeyCode::Char('h')), false),
            Action::Collapse
        );
        assert_eq!(action_for(press(KeyCode::Left), false), Action::Collapse);
        assert_eq!(action_for(press(KeyCode::Char('l')), false), Action::Expand);
        assert_eq!(action_for(press(KeyCode::Right), false), Action::Expand);
        assert_eq!(action_for(press(KeyCode::Tab), false), Action::FocusNext);
        assert_eq!(action_for(press(KeyCode::Enter), false), Action::Activate);
        assert_eq!(action_for(press(KeyCode::Backspace), false), Action::Back);
        assert_eq!(action_for(press(KeyCode::Char('g')), false), Action::Top);
        assert_eq!(action_for(press(KeyCode::Char('G')), false), Action::Bottom);
        assert_eq!(action_for(press(KeyCode::PageUp), false), Action::PageUp);
        assert_eq!(
            action_for(press(KeyCode::PageDown), false),
            Action::PageDown
        );
        assert_eq!(
            action_for(press(KeyCode::Char('d')), false),
            Action::CycleView
        );
        assert_eq!(
            action_for(press(KeyCode::Char('p')), false),
            Action::TogglePreview
        );
        assert_eq!(
            action_for(press(KeyCode::Char('m')), false),
            Action::ToggleMarkdown
        );
        assert_eq!(
            action_for(press(KeyCode::Char('n')), false),
            Action::NextHit
        );
        assert_eq!(
            action_for(press(KeyCode::Char('N')), false),
            Action::PrevHit
        );
        assert_eq!(action_for(press(KeyCode::Char('z')), false), Action::Noop);
    }

    #[test]
    fn search_mode_routes_text_input() {
        assert_eq!(
            action_for(press(KeyCode::Char('q')), true),
            Action::SearchChar('q'),
            "q types into the query instead of quitting"
        );
        assert_eq!(action_for(press(KeyCode::Esc), true), Action::SearchCancel);
        assert_eq!(
            action_for(press(KeyCode::Enter), true),
            Action::SearchAccept
        );
        assert_eq!(
            action_for(press(KeyCode::Backspace), true),
            Action::SearchBackspace
        );
        assert_eq!(action_for(press(KeyCode::Tab), true), Action::Noop);
    }

    #[test]
    fn key_release_is_ignored() {
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(action_for(release, false), Action::Noop);
    }
}
