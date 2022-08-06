mod cursor;
mod layout;
mod tree_blocks;
mod widgets;

use std::sync::Arc;

use async_trait::async_trait;
use crossterm::event::KeyCode;
use parking_lot::FairMutex;
use tokio::sync::Mutex;
use toss::frame::{Frame, Pos, Size};
use toss::terminal::Terminal;

use crate::store::{Msg, MsgStore};
use crate::ui::input::{key, KeyBindingsList, KeyEvent};
use crate::ui::util;
use crate::ui::widgets::editor::EditorState;
use crate::ui::widgets::Widget;

use self::cursor::Cursor;

use super::{ChatMsg, Reaction};

///////////
// State //
///////////

enum Correction {
    MakeCursorVisible,
    MoveCursorToVisibleArea,
}

struct InnerTreeViewState<M: Msg, S: MsgStore<M>> {
    store: S,

    last_cursor: Cursor<M::Id>,
    last_cursor_line: i32,

    cursor: Cursor<M::Id>,

    /// Scroll the view on the next render. Positive values scroll up and
    /// negative values scroll down.
    scroll: i32,
    correction: Option<Correction>,

    editor: EditorState,
}

impl<M: Msg, S: MsgStore<M>> InnerTreeViewState<M, S> {
    fn new(store: S) -> Self {
        Self {
            store,
            last_cursor: Cursor::Bottom,
            last_cursor_line: 0,
            cursor: Cursor::Bottom,
            scroll: 0,
            correction: None,
            editor: EditorState::new(),
        }
    }

    pub fn list_movement_key_bindings(&self, bindings: &mut KeyBindingsList) {
        bindings.binding("j/k, ↓/↑", "move cursor up/down");
        bindings.binding("h/l, ←/→", "move cursor chronologically");
        bindings.binding("g, home", "move cursor to top");
        bindings.binding("G, end", "move cursor to bottom");
        bindings.binding("ctrl+y/e", "scroll up/down a line");
        bindings.binding("ctrl+u/d", "scroll up/down half a screen");
        bindings.binding("ctrl+b/f", "scroll up/down one screen");
    }

    async fn handle_movement_key_event(&mut self, frame: &mut Frame, event: KeyEvent) -> bool {
        let chat_height = frame.size().height - 3;

        match event {
            key!('k') | key!(Up) => self.move_cursor_up().await,
            key!('j') | key!(Down) => self.move_cursor_down().await,
            key!('h') | key!(Left) => self.move_cursor_older().await,
            key!('l') | key!(Right) => self.move_cursor_newer().await,
            key!('g') | key!(Home) => self.move_cursor_to_top().await,
            key!('G') | key!(End) => self.move_cursor_to_bottom().await,
            key!(Ctrl + 'y') => self.scroll_up(1),
            key!(Ctrl + 'e') => self.scroll_down(1),
            key!(Ctrl + 'u') => self.scroll_up((chat_height / 2).into()),
            key!(Ctrl + 'd') => self.scroll_down((chat_height / 2).into()),
            key!(Ctrl + 'b') => self.scroll_up(chat_height.saturating_sub(1).into()),
            key!(Ctrl + 'f') => self.scroll_down(chat_height.saturating_sub(1).into()),
            _ => return false,
        }

        true
    }

    pub fn list_edit_initiating_key_bindings(&self, bindings: &mut KeyBindingsList) {
        bindings.empty();
        bindings.binding("r", "reply to message");
        bindings.binding_ctd("(inline if possible, otherwise directly)");
        bindings.binding("R", "reply to message (opposite of R)");
        bindings.binding("t", "start a new thread");
    }

    async fn handle_edit_initiating_key_event(
        &mut self,
        event: KeyEvent,
        id: Option<M::Id>,
    ) -> bool {
        match event {
            key!('r') => {
                if let Some(parent) = self.parent_for_normal_reply().await {
                    self.cursor = Cursor::editor(id, parent);
                    self.correction = Some(Correction::MakeCursorVisible);
                }
            }
            key!('R') => {
                if let Some(parent) = self.parent_for_alternate_reply().await {
                    self.cursor = Cursor::editor(id, parent);
                    self.correction = Some(Correction::MakeCursorVisible);
                }
            }
            key!('t') | key!('T') => {
                self.cursor = Cursor::editor(id, None);
                self.correction = Some(Correction::MakeCursorVisible);
            }
            _ => return false,
        }

        true
    }

    pub fn list_normal_key_bindings(&self, bindings: &mut KeyBindingsList, can_compose: bool) {
        self.list_movement_key_bindings(bindings);
        if can_compose {
            self.list_edit_initiating_key_bindings(bindings);
        }
    }

    async fn handle_normal_key_event(
        &mut self,
        frame: &mut Frame,
        event: KeyEvent,
        can_compose: bool,
        id: Option<M::Id>,
    ) -> bool {
        if self.handle_movement_key_event(frame, event).await {
            true
        } else if can_compose {
            self.handle_edit_initiating_key_event(event, id).await
        } else {
            false
        }
    }

    fn list_editor_key_bindings(&self, bindings: &mut KeyBindingsList) {
        bindings.binding("esc", "close editor");
        bindings.binding("enter", "send message");
        util::list_editor_key_bindings(bindings, |_| true, true);
    }

    fn handle_editor_key_event(
        &mut self,
        terminal: &mut Terminal,
        crossterm_lock: &Arc<FairMutex<()>>,
        event: KeyEvent,
        coming_from: Option<M::Id>,
        parent: Option<M::Id>,
    ) -> Reaction<M> {
        // TODO Tab-completion
        match event {
            key!(Esc) => {
                self.cursor = coming_from.map(Cursor::Msg).unwrap_or(Cursor::Bottom);
                return Reaction::Handled;
            }

            key!(Enter) => {
                let content = self.editor.text();
                if !content.trim().is_empty() {
                    self.cursor = Cursor::Pseudo {
                        coming_from,
                        parent: parent.clone(),
                    };
                    return Reaction::Composed { parent, content };
                }
            }

            _ => {
                let handled = util::handle_editor_key_event(
                    &self.editor,
                    terminal,
                    crossterm_lock,
                    event,
                    |_| true,
                    true,
                );
                if !handled {
                    return Reaction::NotHandled;
                }
            }
        }

        self.correction = Some(Correction::MakeCursorVisible);
        Reaction::Handled
    }

    pub fn list_key_bindings(&self, bindings: &mut KeyBindingsList, can_compose: bool) {
        bindings.heading("Chat");
        match &self.cursor {
            Cursor::Bottom | Cursor::Msg(_) => {
                self.list_normal_key_bindings(bindings, can_compose);
            }
            Cursor::Editor { .. } => self.list_editor_key_bindings(bindings),
            Cursor::Pseudo { .. } => {
                self.list_movement_key_bindings(bindings);
            }
        }
    }

    async fn handle_key_event(
        &mut self,
        terminal: &mut Terminal,
        crossterm_lock: &Arc<FairMutex<()>>,
        event: KeyEvent,
        can_compose: bool,
    ) -> Reaction<M> {
        match &self.cursor {
            Cursor::Bottom => {
                if self
                    .handle_normal_key_event(terminal.frame(), event, can_compose, None)
                    .await
                {
                    Reaction::Handled
                } else {
                    Reaction::NotHandled
                }
            }
            Cursor::Msg(id) => {
                let id = id.clone();
                if self
                    .handle_normal_key_event(terminal.frame(), event, can_compose, Some(id))
                    .await
                {
                    Reaction::Handled
                } else {
                    Reaction::NotHandled
                }
            }
            Cursor::Editor {
                coming_from,
                parent,
            } => self.handle_editor_key_event(
                terminal,
                crossterm_lock,
                event,
                coming_from.clone(),
                parent.clone(),
            ),
            Cursor::Pseudo { .. } => {
                if self
                    .handle_movement_key_event(terminal.frame(), event)
                    .await
                {
                    Reaction::Handled
                } else {
                    Reaction::NotHandled
                }
            }
        }
    }

    fn sent(&mut self, id: Option<M::Id>) {
        if let Cursor::Pseudo { coming_from, .. } = &self.cursor {
            if let Some(id) = id {
                self.last_cursor = Cursor::Msg(id.clone());
                self.cursor = Cursor::Msg(id);
                self.editor.clear();
            } else {
                self.cursor = match coming_from {
                    Some(id) => Cursor::Msg(id.clone()),
                    None => Cursor::Bottom,
                };
            };
        }
    }
}

pub struct TreeViewState<M: Msg, S: MsgStore<M>>(Arc<Mutex<InnerTreeViewState<M, S>>>);

impl<M: Msg, S: MsgStore<M>> TreeViewState<M, S> {
    pub fn new(store: S) -> Self {
        Self(Arc::new(Mutex::new(InnerTreeViewState::new(store))))
    }

    pub fn widget(&self, nick: String) -> TreeView<M, S> {
        TreeView {
            inner: self.0.clone(),
            nick,
        }
    }

    pub async fn list_key_bindings(&self, bindings: &mut KeyBindingsList, can_compose: bool) {
        self.0.lock().await.list_key_bindings(bindings, can_compose);
    }

    pub async fn handle_key_event(
        &mut self,
        terminal: &mut Terminal,
        crossterm_lock: &Arc<FairMutex<()>>,
        event: KeyEvent,
        can_compose: bool,
    ) -> Reaction<M> {
        self.0
            .lock()
            .await
            .handle_key_event(terminal, crossterm_lock, event, can_compose)
            .await
    }

    pub async fn sent(&mut self, id: Option<M::Id>) {
        self.0.lock().await.sent(id)
    }
}

////////////
// Widget //
////////////

pub struct TreeView<M: Msg, S: MsgStore<M>> {
    inner: Arc<Mutex<InnerTreeViewState<M, S>>>,
    nick: String,
}

#[async_trait]
impl<M, S> Widget for TreeView<M, S>
where
    M: Msg + ChatMsg,
    M::Id: Send + Sync,
    S: MsgStore<M> + Send + Sync,
{
    fn size(&self, _frame: &mut Frame, _max_width: Option<u16>, _max_height: Option<u16>) -> Size {
        Size::ZERO
    }

    async fn render(self: Box<Self>, frame: &mut Frame) {
        let mut guard = self.inner.lock().await;
        let blocks = guard.relayout(&self.nick, frame).await;

        let size = frame.size();
        for block in blocks.into_blocks().blocks {
            frame.push(
                Pos::new(0, block.top_line),
                Size::new(size.width, block.height as u16),
            );
            block.widget.render(frame).await;
            frame.pop();
        }
    }
}
