use gio::prelude::*;
use gio::SimpleActionGroup;
use std::cell::Ref;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;

use crate::app::components::{labels, PlaylistModel, SelectionTool, SelectionToolsModel};
use crate::app::models::SongDescription;
use crate::app::models::SongModel;
use crate::app::state::PlaylistChange;
use crate::app::state::{
    PlaybackAction, PlaybackEvent, PlaybackState, SelectionAction, SelectionContext, SelectionState,
};
use crate::app::{ActionDispatcher, AppAction, AppEvent, AppModel, AppState, ListDiff};
use crate::{api::SpotifyApiClient, app::components::SimpleSelectionTool};

pub struct NowPlayingModel {
    app_model: Rc<AppModel>,
    dispatcher: Box<dyn ActionDispatcher>,
}

impl NowPlayingModel {
    pub fn new(app_model: Rc<AppModel>, dispatcher: Box<dyn ActionDispatcher>) -> Self {
        Self {
            app_model,
            dispatcher,
        }
    }

    fn state(&self) -> Ref<'_, AppState> {
        self.app_model.get_state()
    }

    fn queue(&self) -> Ref<'_, PlaybackState> {
        Ref::map(self.state(), |s| &s.playback)
    }

    pub fn load_more(&self) -> Option<()> {
        let queue = self.queue();
        let loader = self.app_model.get_batch_loader();
        let query = queue.next_query()?;

        self.dispatcher.dispatch_async(Box::pin(async move {
            let source = query.source.clone();
            let action = loader
                .query(query, |song_batch| {
                    PlaybackAction::LoadPagedSongs(source, song_batch).into()
                })
                .await;
            Some(action)
        }));

        Some(())
    }
}

impl PlaylistModel for NowPlayingModel {
    fn current_song_id(&self) -> Option<String> {
        self.queue().current_song_id().cloned()
    }

    fn play_song_at(&self, _pos: usize, id: &str) {
        self.dispatcher
            .dispatch(PlaybackAction::Load(id.to_string()).into());
    }

    fn diff_for_event(&self, event: &AppEvent) -> Option<ListDiff<SongModel>> {
        let queue = self.queue();
        let songs = queue.songs().map(|s| s.into());

        match event {
            AppEvent::PlaybackEvent(PlaybackEvent::PlaylistChanged(change)) => match change {
                PlaylistChange::Reset => Some(ListDiff::Set(songs.collect())),
                PlaylistChange::InsertedAt(i, n) => {
                    Some(ListDiff::Insert(*i, songs.skip(*i).take(*n).collect()))
                }
                PlaylistChange::AppendedAt(i) => Some(ListDiff::Append(songs.skip(*i).collect())),
                PlaylistChange::MovedDown(i) => Some(ListDiff::MoveDown(*i)),
                PlaylistChange::MovedUp(i) => Some(ListDiff::MoveUp(*i)),
            },
            _ => None,
        }
    }

    fn autoscroll_to_playing(&self) -> bool {
        true
    }

    fn actions_for(&self, id: &str) -> Option<gio::ActionGroup> {
        let queue = self.queue();
        let song = queue.song(id)?;
        let group = SimpleActionGroup::new();

        for view_artist in song.make_artist_actions(self.dispatcher.box_clone(), None) {
            group.add_action(&view_artist);
        }
        group.add_action(&song.make_album_action(self.dispatcher.box_clone(), None));
        group.add_action(&song.make_link_action(None));
        group.add_action(&song.make_dequeue_action(self.dispatcher.box_clone(), None));

        Some(group.upcast())
    }

    fn menu_for(&self, id: &str) -> Option<gio::MenuModel> {
        let queue = self.queue();
        let song = queue.song(id)?;

        let menu = gio::Menu::new();
        menu.append(Some(&*labels::VIEW_ALBUM), Some("song.view_album"));
        for artist in song.artists.iter() {
            menu.append(
                Some(&format!(
                    "{} {}",
                    *labels::MORE_FROM,
                    glib::markup_escape_text(&artist.name)
                )),
                Some(&format!("song.view_artist_{}", artist.id)),
            );
        }

        menu.append(Some(&*labels::COPY_LINK), Some("song.copy_link"));
        menu.append(Some(&*labels::REMOVE_FROM_QUEUE), Some("song.dequeue"));

        Some(menu.upcast())
    }

    fn select_song(&self, id: &str) {
        let queue = self.queue();
        if let Some(song) = queue.song(id) {
            self.dispatcher
                .dispatch(SelectionAction::Select(vec![song.clone()]).into());
        }
    }

    fn deselect_song(&self, id: &str) {
        self.dispatcher
            .dispatch(SelectionAction::Deselect(vec![id.to_string()]).into());
    }

    fn enable_selection(&self) -> bool {
        self.dispatcher
            .dispatch(AppAction::ChangeSelectionMode(true));
        true
    }

    fn selection(&self) -> Option<Box<dyn Deref<Target = SelectionState> + '_>> {
        let selection = self
            .app_model
            .map_state_opt(|s| Some(&s.selection))
            .filter(|s| s.context == SelectionContext::Queue)?;
        Some(Box::new(selection))
    }
}

impl SelectionToolsModel for NowPlayingModel {
    fn dispatcher(&self) -> Box<dyn ActionDispatcher> {
        self.dispatcher.box_clone()
    }

    fn spotify_client(&self) -> Arc<dyn SpotifyApiClient + Send + Sync> {
        self.app_model.get_spotify()
    }

    fn selection(&self) -> Option<Box<dyn Deref<Target = SelectionState> + '_>> {
        let selection = self
            .app_model
            .map_state_opt(|s| Some(&s.selection))
            .filter(|s| s.context == SelectionContext::Queue)?;
        Some(Box::new(selection))
    }

    fn tools_visible(&self, _: &SelectionState) -> Vec<SelectionTool> {
        vec![
            SelectionTool::Simple(SimpleSelectionTool::SelectAll),
            SelectionTool::Simple(SimpleSelectionTool::MoveDown),
            SelectionTool::Simple(SimpleSelectionTool::MoveUp),
            SelectionTool::Simple(SimpleSelectionTool::Remove),
        ]
    }

    fn handle_tool_activated(&self, selection: &SelectionState, tool: &SelectionTool) {
        match tool {
            SelectionTool::Simple(SimpleSelectionTool::SelectAll) => {
                let queue = self.queue();
                let songs = queue.songs().collect::<Vec<&SongDescription>>();
                self.handle_select_all_tool_borrowed(selection, &songs);
            }
            SelectionTool::Simple(SimpleSelectionTool::Remove) => {
                self.dispatcher().dispatch(AppAction::DequeueSelection);
            }
            SelectionTool::Simple(SimpleSelectionTool::MoveDown) => {
                self.dispatcher().dispatch(AppAction::MoveDownSelection);
            }
            SelectionTool::Simple(SimpleSelectionTool::MoveUp) => {
                self.dispatcher().dispatch(AppAction::MoveUpSelection);
            }
            _ => self.default_handle_tool_activated(selection, tool),
        };
    }
}
