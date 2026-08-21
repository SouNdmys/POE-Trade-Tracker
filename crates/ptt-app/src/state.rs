//! What a page is showing, and the controls it keeps between frames.
//!
//! Two kinds of state live here. The first is the answer a page is currently
//! displaying, which arrives from a background task and may be a failure or
//! an absence rather than content — states the interface has to draw, so they
//! are variants rather than an empty vector.
//!
//! The second is the control entities. GPUI inputs and selects own their text
//! and their cursor, so they have to outlive a render: rebuilding one on
//! refresh throws away whatever the user was in the middle of typing.

use ptt_settings::UiLanguage;

/// A page's answer.
#[derive(Clone, Debug, Default)]
pub enum PageData {
    /// The page draws its own state and reads nothing from the store.
    #[default]
    Empty,
    /// A pair page with no pair yet.
    WaitingForPair,
    /// The store is being read. Kept distinct from an empty answer, which is
    /// a claim about the market rather than about the reader's patience.
    Loading,
    /// Display lines, for the pages the interface has not upgraded yet.
    Text(Vec<String>),
    /// The probe queue, which the monitor draws as rows rather than lines so
    /// each pair carries its own controls.
    Probes(Box<ptt_runtime::reports::ProbeQueueModel>),
    /// The radar's ranked routes.
    Opportunities(Box<ptt_runtime::reports::OpportunitiesModel>),
    /// "I hold X and want Y", priced.
    Convert(Box<ptt_runtime::reports::ConvertModel>),
    Failed(String),
}

impl PageData {
    /// Whether the page has nothing to draw beyond its own message.
    #[must_use]
    pub fn is_content(&self) -> bool {
        match self {
            Self::Text(lines) => !lines.is_empty(),
            Self::Probes(_) | Self::Opportunities(_) | Self::Convert(_) => true,
            _ => false,
        }
    }
}

/// A probe the user asked for, as opposed to one the radar suggested.
///
/// Held for the session rather than stored: the queue's whole purpose is to
/// empty itself as pairs get captured, so an entry that outlived the data it
/// was asking for would be a to-do item that never completes. Persisting the
/// lifecycle is a separate decision, deferred with the rest of `ProbeStatus`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedProbe {
    pub from_asset_id: String,
    pub to_asset_id: String,
    /// Why it was worth pinning, already in the reader's language: the reason
    /// describes the moment it was pinned, and re-deriving it later would
    /// describe a different one.
    pub reason: String,
}

impl PinnedProbe {
    #[must_use]
    pub fn matches(&self, from: &str, to: &str) -> bool {
        self.from_asset_id == from && self.to_asset_id == to
    }
}

/// The pinned queue, and the rule that empties it.
#[derive(Clone, Debug, Default)]
pub struct ProbeQueue {
    pinned: Vec<PinnedProbe>,
}

impl ProbeQueue {
    #[must_use]
    pub fn entries(&self) -> &[PinnedProbe] {
        &self.pinned
    }

    #[must_use]
    pub fn is_pinned(&self, from: &str, to: &str) -> bool {
        self.pinned.iter().any(|entry| entry.matches(from, to))
    }

    /// Adds a pair, or moves it to the front if it is already queued.
    pub fn pin(&mut self, entry: PinnedProbe) {
        self.pinned
            .retain(|held| !held.matches(&entry.from_asset_id, &entry.to_asset_id));
        self.pinned.insert(0, entry);
    }

    pub fn unpin(&mut self, from: &str, to: &str) {
        self.pinned.retain(|entry| !entry.matches(from, to));
    }

    /// Drops every pinned pair the book can now answer.
    ///
    /// This is the closing half of the loop: a suggestion sends the user to a
    /// pair in game, the watcher ingests it, and the suggestion has to
    /// disappear on its own — a queue that only grows is a queue nobody
    /// reads. `still_missing` is the set the coverage pass says is still
    /// incomplete, so anything absent from it has been answered.
    pub fn retain_missing(&mut self, still_missing: &[(String, String)]) {
        self.pinned.retain(|entry| {
            still_missing
                .iter()
                .any(|(from, to)| entry.matches(from, to))
        });
    }
}

/// The word for a page that is still loading.
#[must_use]
pub const fn loading_label(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::English => "reading the book…",
        UiLanguage::Chinese => "正在读取盘口…",
    }
}
