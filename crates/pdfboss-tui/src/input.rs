//! Key-event → intent mapping. Pure so bindings are unit-testable.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::yank::YankTarget;

/// Where key events route: the search input and the yank menu capture
/// keys before the normal bindings apply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyContext {
    Normal,
    Search,
    Yank,
}

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
    OpenYank,
    Yank(YankTarget),
    YankCancel,
    /// Move the pane divider the arrow points at: left/right the vertical
    /// tree divider, up/down the inspector/hex divider.
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    Quit,
    Noop,
}

/// Maps a key event within `context`: search reroutes printable keys
/// into the status-bar query (Esc cancels the search instead of
/// quitting); the yank menu maps its target keys and cancels on
/// anything else.
pub fn action_for(key: KeyEvent, context: KeyContext) -> Action {
    if key.kind != KeyEventKind::Press {
        return Action::Noop;
    }
    if context == KeyContext::Search {
        return match key.code {
            KeyCode::Esc => Action::SearchCancel,
            KeyCode::Enter => Action::SearchAccept,
            KeyCode::Backspace => Action::SearchBackspace,
            KeyCode::Char(c) => Action::SearchChar(c),
            _ => Action::Noop,
        };
    }
    if context == KeyContext::Yank {
        return match key.code {
            KeyCode::Char('q') => Action::Yank(YankTarget::Query),
            KeyCode::Char('c') => Action::Yank(YankTarget::Command),
            KeyCode::Char('x') => Action::Yank(YankTarget::Hexdump),
            KeyCode::Char('b') => Action::Yank(YankTarget::Bytes),
            KeyCode::Char('e') => Action::Yank(YankTarget::Element),
            KeyCode::Char('m') => Action::Yank(YankTarget::Markdown),
            KeyCode::Char('o') => Action::Yank(YankTarget::ObjRef),
            _ => Action::YankCancel,
        };
    }
    // Alt+arrows (Option on macOS) resize the panes. Ctrl+arrows stay as
    // a fallback for terminals that swallow the Alt modifier, matched on
    // Ctrl alone because several terminals report Ctrl+Shift+arrow
    // without the Shift bit; neither chord shadows anything.
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Left => return Action::ResizeLeft,
            KeyCode::Right => return Action::ResizeRight,
            KeyCode::Up => return Action::ResizeUp,
            KeyCode::Down => return Action::ResizeDown,
            _ => {}
        }
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
        KeyCode::Char('y') => Action::OpenYank,
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
        assert_eq!(
            action_for(press(KeyCode::Char('q')), KeyContext::Normal),
            Action::Quit
        );
        assert_eq!(
            action_for(press(KeyCode::Esc), KeyContext::Normal),
            Action::Quit
        );
        assert_eq!(
            action_for(press(KeyCode::Char('/')), KeyContext::Normal),
            Action::OpenSearch
        );
        assert_eq!(
            action_for(press(KeyCode::Char('j')), KeyContext::Normal),
            Action::MoveDown
        );
        assert_eq!(
            action_for(press(KeyCode::Down), KeyContext::Normal),
            Action::MoveDown
        );
        assert_eq!(
            action_for(press(KeyCode::Char('k')), KeyContext::Normal),
            Action::MoveUp
        );
        assert_eq!(
            action_for(press(KeyCode::Up), KeyContext::Normal),
            Action::MoveUp
        );
        assert_eq!(
            action_for(press(KeyCode::Char('h')), KeyContext::Normal),
            Action::Collapse
        );
        assert_eq!(
            action_for(press(KeyCode::Left), KeyContext::Normal),
            Action::Collapse
        );
        assert_eq!(
            action_for(press(KeyCode::Char('l')), KeyContext::Normal),
            Action::Expand
        );
        assert_eq!(
            action_for(press(KeyCode::Right), KeyContext::Normal),
            Action::Expand
        );
        assert_eq!(
            action_for(press(KeyCode::Tab), KeyContext::Normal),
            Action::FocusNext
        );
        assert_eq!(
            action_for(press(KeyCode::Enter), KeyContext::Normal),
            Action::Activate
        );
        assert_eq!(
            action_for(press(KeyCode::Backspace), KeyContext::Normal),
            Action::Back
        );
        assert_eq!(
            action_for(press(KeyCode::Char('g')), KeyContext::Normal),
            Action::Top
        );
        assert_eq!(
            action_for(press(KeyCode::Char('G')), KeyContext::Normal),
            Action::Bottom
        );
        assert_eq!(
            action_for(press(KeyCode::PageUp), KeyContext::Normal),
            Action::PageUp
        );
        assert_eq!(
            action_for(press(KeyCode::PageDown), KeyContext::Normal),
            Action::PageDown
        );
        assert_eq!(
            action_for(press(KeyCode::Char('d')), KeyContext::Normal),
            Action::CycleView
        );
        assert_eq!(
            action_for(press(KeyCode::Char('p')), KeyContext::Normal),
            Action::TogglePreview
        );
        assert_eq!(
            action_for(press(KeyCode::Char('m')), KeyContext::Normal),
            Action::ToggleMarkdown
        );
        assert_eq!(
            action_for(press(KeyCode::Char('n')), KeyContext::Normal),
            Action::NextHit
        );
        assert_eq!(
            action_for(press(KeyCode::Char('N')), KeyContext::Normal),
            Action::PrevHit
        );
        assert_eq!(
            action_for(press(KeyCode::Char('z')), KeyContext::Normal),
            Action::Noop
        );
    }

    #[test]
    fn search_mode_routes_text_input() {
        assert_eq!(
            action_for(press(KeyCode::Char('q')), KeyContext::Search),
            Action::SearchChar('q'),
            "q types into the query instead of quitting"
        );
        assert_eq!(
            action_for(press(KeyCode::Esc), KeyContext::Search),
            Action::SearchCancel
        );
        assert_eq!(
            action_for(press(KeyCode::Enter), KeyContext::Search),
            Action::SearchAccept
        );
        assert_eq!(
            action_for(press(KeyCode::Backspace), KeyContext::Search),
            Action::SearchBackspace
        );
        assert_eq!(
            action_for(press(KeyCode::Tab), KeyContext::Search),
            Action::Noop
        );
    }

    #[test]
    fn y_opens_the_yank_menu() {
        assert_eq!(
            action_for(press(KeyCode::Char('y')), KeyContext::Normal),
            Action::OpenYank
        );
    }

    #[test]
    fn yank_mode_maps_targets_and_cancels_on_anything_else() {
        for (code, target) in [
            (KeyCode::Char('q'), YankTarget::Query),
            (KeyCode::Char('c'), YankTarget::Command),
            (KeyCode::Char('x'), YankTarget::Hexdump),
            (KeyCode::Char('b'), YankTarget::Bytes),
            (KeyCode::Char('e'), YankTarget::Element),
            (KeyCode::Char('m'), YankTarget::Markdown),
            (KeyCode::Char('o'), YankTarget::ObjRef),
        ] {
            assert_eq!(
                action_for(press(code), KeyContext::Yank),
                Action::Yank(target)
            );
        }
        assert_eq!(
            action_for(press(KeyCode::Esc), KeyContext::Yank),
            Action::YankCancel
        );
        assert_eq!(
            action_for(press(KeyCode::Char('z')), KeyContext::Yank),
            Action::YankCancel,
            "an unmapped key closes the menu instead of leaking through"
        );
        assert_eq!(
            action_for(press(KeyCode::Char('v')), KeyContext::Yank),
            Action::YankCancel,
            "v moved to e; the old key cancels instead of copying"
        );
        assert_eq!(
            action_for(press(KeyCode::Enter), KeyContext::Yank),
            Action::YankCancel
        );
    }

    #[test]
    fn alt_arrows_resize() {
        for (code, action) in [
            (KeyCode::Left, Action::ResizeLeft),
            (KeyCode::Right, Action::ResizeRight),
            (KeyCode::Up, Action::ResizeUp),
            (KeyCode::Down, Action::ResizeDown),
        ] {
            assert_eq!(
                action_for(KeyEvent::new(code, KeyModifiers::ALT), KeyContext::Normal),
                action
            );
        }
    }

    #[test]
    fn ctrl_shift_arrows_resize() {
        let chord = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        for (code, action) in [
            (KeyCode::Left, Action::ResizeLeft),
            (KeyCode::Right, Action::ResizeRight),
            (KeyCode::Up, Action::ResizeUp),
            (KeyCode::Down, Action::ResizeDown),
        ] {
            assert_eq!(
                action_for(KeyEvent::new(code, chord), KeyContext::Normal),
                action
            );
            // Terminals that drop the Shift bit still resize on Ctrl+arrow.
            assert_eq!(
                action_for(
                    KeyEvent::new(code, KeyModifiers::CONTROL),
                    KeyContext::Normal
                ),
                action
            );
        }
        // Shift alone keeps the plain movement meaning.
        assert_eq!(
            action_for(
                KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT),
                KeyContext::Normal
            ),
            Action::Collapse
        );
    }

    #[test]
    fn key_release_is_ignored() {
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(action_for(release, KeyContext::Normal), Action::Noop);
    }
}
