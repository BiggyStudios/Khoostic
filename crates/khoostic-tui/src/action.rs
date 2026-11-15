//! Everything a key can do. Names are what users write in `config.toml` and `:map`.

macro_rules! actions {
    ($( $variant:ident = $name:literal : $help:literal ),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Action { $( $variant ),* }

        impl Action {
            pub const ALL: &'static [Action] = &[ $( Action::$variant ),* ];

            pub fn name(self) -> &'static str {
                match self { $( Action::$variant => $name ),* }
            }

            pub fn help(self) -> &'static str {
                match self { $( Action::$variant => $help ),* }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name { $( $name => Some(Action::$variant), )* _ => None }
            }
        }
    };
}

actions! {
    // Motions (take counts)
    MoveDown = "move_down": "cursor down",
    MoveUp = "move_up": "cursor up",
    Top = "top": "first item (or item N with a count)",
    Bottom = "bottom": "last item (or item N with a count)",
    HalfPageDown = "half_page_down": "half page down",
    HalfPageUp = "half_page_up": "half page up",
    PageDown = "page_down": "page down",
    PageUp = "page_up": "page up",
    GotoPlaying = "goto_playing": "jump cursor to the playing track",

    // Items
    Activate = "activate": "play track / open folder",
    Parent = "parent": "browser: go to parent folder",
    Open = "open": "browser: open folder",
    Append = "append": "add to end of queue",
    InsertNext = "insert_next": "play after the current track",
    Delete = "delete": "remove from queue",
    Yank = "yank": "copy tracks",
    PasteAfter = "paste_after": "paste copied tracks below",
    PasteBefore = "paste_before": "paste copied tracks above",
    MoveItemDown = "move_item_down": "move queue entry down",
    MoveItemUp = "move_item_up": "move queue entry up",
    VisualMode = "visual": "start/stop visual selection",
    Escape = "escape": "cancel / leave mode",

    // Playback
    TogglePause = "toggle_pause": "play / pause",
    Stop = "stop": "stop",
    NextTrack = "next_track": "next track",
    PrevTrack = "prev_track": "previous track (restarts if > 3s in)",
    SeekForward = "seek_forward": "seek forward",
    SeekBackward = "seek_backward": "seek backward",
    SeekForwardBig = "seek_forward_big": "seek forward a lot",
    SeekBackwardBig = "seek_backward_big": "seek backward a lot",
    SeekStart = "seek_start": "restart track",
    VolumeUp = "volume_up": "volume up",
    VolumeDown = "volume_down": "volume down",
    ToggleShuffle = "toggle_shuffle": "shuffle on/off",
    CycleRepeat = "cycle_repeat": "repeat off → all → one",

    // UI
    CommandMode = "command": "command line",
    Search = "search": "search in current view",
    SearchNext = "search_next": "next search match",
    SearchPrev = "search_prev": "previous search match",
    NextView = "next_view": "next view",
    PrevView = "prev_view": "previous view",
    ViewQueue = "view_queue": "queue view",
    ViewBrowse = "view_browse": "browser view",
    ViewVisualizer = "view_visualizer": "visualizer view",
    CycleVisualizer = "cycle_visualizer": "next visualizer style",
    Help = "help": "toggle help",
    Quit = "quit": "quit",
}

impl Action {
    /// Whether a count prefix repeats/scales the action.
    pub fn takes_count(self) -> bool {
        use Action::*;
        matches!(
            self,
            MoveDown
                | MoveUp
                | Top
                | Bottom
                | HalfPageDown
                | HalfPageUp
                | PageDown
                | PageUp
                | Delete
                | Yank
                | PasteAfter
                | PasteBefore
                | MoveItemDown
                | MoveItemUp
                | NextTrack
                | PrevTrack
                | SeekForward
                | SeekBackward
                | SeekForwardBig
                | SeekBackwardBig
                | VolumeUp
                | VolumeDown
                | SearchNext
                | SearchPrev
                | Append
                | InsertNext
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for &a in Action::ALL {
            assert!(seen.insert(a.name()), "duplicate name {}", a.name());
            assert_eq!(Action::from_name(a.name()), Some(a));
        }
    }
}
