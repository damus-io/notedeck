//! Calendar application for Notedeck.
//!
//! This module provides the main `CalendarApp` struct that implements
//! the `notedeck::App` trait for integration with the Notedeck chrome.

use chrono::{Datelike, Local, NaiveDate, TimeZone};
use egui::{Color32, CornerRadius, RichText, Ui, Vec2};
use nostrdb::{Ndb, NoteKey, Subscription, Transaction};
use notedeck::enostr::ClientMessage;
use notedeck::media::{AnimationMode, ImageType};
use notedeck::name::get_display_name;
use notedeck::profile::get_profile_url;
use notedeck::{
    try_process_events_core, App, AppContext, AppResponse, Images, IsFollowing, MediaJobs,
};
use std::collections::HashSet;
use uuid::Uuid;

use crate::comment::{CachedComment, Comment, KIND_COMMENT};
use crate::subscription::{calendar_comments_filter, calendar_events_filter};
use crate::timezone::format_time;
use crate::{CalendarEvent, CalendarTime, KIND_DATE_CALENDAR_EVENT, KIND_TIME_CALENDAR_EVENT};

/// Extract a country flag emoji from a location string.
///
/// This function attempts to identify a country from the location string
/// and returns the corresponding flag emoji. It handles various formats:
/// - Full country names: "Canada", "United States", "Germany"
/// - Country codes: "US", "CA", "DE"
/// - City/country combinations: "Edmonton, AB, Canada", "Berlin, Germany"
///
/// Returns an empty string if no country is detected.
///
/// NOTE: Currently unused - egui doesn't support flag emoji rendering.
/// See bead notedeck_calendar-emj.
#[allow(dead_code)]
fn country_flag_from_location(location: &str) -> &'static str {
    let location_lower = location.to_lowercase();

    // Check for country names and codes (ordered by likely frequency in Nostr events)
    // United States variants
    if location_lower.contains("united states")
        || location_lower.contains("usa")
        || location_lower.ends_with(", us")
        || location_lower == "us"
        || location_lower.contains("u.s.a")
        || location_lower.contains("u.s.")
    {
        return "\u{1F1FA}\u{1F1F8}"; // US flag
    }

    // Canada
    if location_lower.contains("canada")
        || location_lower.ends_with(", ca")
        || location_lower == "ca"
    {
        return "\u{1F1E8}\u{1F1E6}"; // CA flag
    }

    // Germany
    if location_lower.contains("germany")
        || location_lower.contains("deutschland")
        || location_lower.ends_with(", de")
        || location_lower == "de"
    {
        return "\u{1F1E9}\u{1F1EA}"; // DE flag
    }

    // United Kingdom variants
    if location_lower.contains("united kingdom")
        || location_lower.contains("england")
        || location_lower.contains("scotland")
        || location_lower.contains("wales")
        || location_lower.ends_with(", uk")
        || location_lower.ends_with(", gb")
        || location_lower == "uk"
        || location_lower == "gb"
    {
        return "\u{1F1EC}\u{1F1E7}"; // GB flag
    }

    // Japan
    if location_lower.contains("japan")
        || location_lower.ends_with(", jp")
        || location_lower == "jp"
    {
        return "\u{1F1EF}\u{1F1F5}"; // JP flag
    }

    // France
    if location_lower.contains("france")
        || location_lower.ends_with(", fr")
        || location_lower == "fr"
    {
        return "\u{1F1EB}\u{1F1F7}"; // FR flag
    }

    // Spain
    if location_lower.contains("spain")
        || location_lower.contains("espana")
        || location_lower.ends_with(", es")
        || location_lower == "es"
    {
        return "\u{1F1EA}\u{1F1F8}"; // ES flag
    }

    // Italy
    if location_lower.contains("italy")
        || location_lower.contains("italia")
        || location_lower.ends_with(", it")
        || location_lower == "it"
    {
        return "\u{1F1EE}\u{1F1F9}"; // IT flag
    }

    // Netherlands
    if location_lower.contains("netherlands")
        || location_lower.contains("holland")
        || location_lower.ends_with(", nl")
        || location_lower == "nl"
    {
        return "\u{1F1F3}\u{1F1F1}"; // NL flag
    }

    // Australia
    if location_lower.contains("australia")
        || location_lower.ends_with(", au")
        || location_lower == "au"
    {
        return "\u{1F1E6}\u{1F1FA}"; // AU flag
    }

    // Brazil
    if location_lower.contains("brazil")
        || location_lower.contains("brasil")
        || location_lower.ends_with(", br")
        || location_lower == "br"
    {
        return "\u{1F1E7}\u{1F1F7}"; // BR flag
    }

    // Mexico
    if location_lower.contains("mexico")
        || location_lower.ends_with(", mx")
        || location_lower == "mx"
    {
        return "\u{1F1F2}\u{1F1FD}"; // MX flag
    }

    // Argentina
    if location_lower.contains("argentina")
        || location_lower.ends_with(", ar")
        || location_lower == "ar"
    {
        return "\u{1F1E6}\u{1F1F7}"; // AR flag
    }

    // Switzerland
    if location_lower.contains("switzerland")
        || location_lower.contains("schweiz")
        || location_lower.contains("suisse")
        || location_lower.ends_with(", ch")
        || location_lower == "ch"
    {
        return "\u{1F1E8}\u{1F1ED}"; // CH flag
    }

    // Austria
    if location_lower.contains("austria")
        || location_lower.ends_with(", at")
        || location_lower == "at"
    {
        return "\u{1F1E6}\u{1F1F9}"; // AT flag
    }

    // Belgium
    if location_lower.contains("belgium")
        || location_lower.ends_with(", be")
        || location_lower == "be"
    {
        return "\u{1F1E7}\u{1F1EA}"; // BE flag
    }

    // Portugal
    if location_lower.contains("portugal")
        || location_lower.ends_with(", pt")
        || location_lower == "pt"
    {
        return "\u{1F1F5}\u{1F1F9}"; // PT flag
    }

    // Sweden
    if location_lower.contains("sweden")
        || location_lower.ends_with(", se")
        || location_lower == "se"
    {
        return "\u{1F1F8}\u{1F1EA}"; // SE flag
    }

    // Norway
    if location_lower.contains("norway")
        || location_lower.ends_with(", no")
        || location_lower == "no"
    {
        return "\u{1F1F3}\u{1F1F4}"; // NO flag
    }

    // Denmark
    if location_lower.contains("denmark")
        || location_lower.ends_with(", dk")
        || location_lower == "dk"
    {
        return "\u{1F1E9}\u{1F1F0}"; // DK flag
    }

    // Finland
    if location_lower.contains("finland")
        || location_lower.ends_with(", fi")
        || location_lower == "fi"
    {
        return "\u{1F1EB}\u{1F1EE}"; // FI flag
    }

    // Poland
    if location_lower.contains("poland")
        || location_lower.contains("polska")
        || location_lower.ends_with(", pl")
        || location_lower == "pl"
    {
        return "\u{1F1F5}\u{1F1F1}"; // PL flag
    }

    // Czech Republic
    if location_lower.contains("czech")
        || location_lower.ends_with(", cz")
        || location_lower == "cz"
    {
        return "\u{1F1E8}\u{1F1FF}"; // CZ flag
    }

    // Ireland
    if location_lower.contains("ireland")
        || location_lower.ends_with(", ie")
        || location_lower == "ie"
    {
        return "\u{1F1EE}\u{1F1EA}"; // IE flag
    }

    // New Zealand
    if location_lower.contains("new zealand")
        || location_lower.ends_with(", nz")
        || location_lower == "nz"
    {
        return "\u{1F1F3}\u{1F1FF}"; // NZ flag
    }

    // Singapore
    if location_lower.contains("singapore")
        || location_lower.ends_with(", sg")
        || location_lower == "sg"
    {
        return "\u{1F1F8}\u{1F1EC}"; // SG flag
    }

    // South Korea
    if location_lower.contains("south korea")
        || location_lower.contains("korea")
        || location_lower.ends_with(", kr")
        || location_lower == "kr"
    {
        return "\u{1F1F0}\u{1F1F7}"; // KR flag
    }

    // China
    if location_lower.contains("china")
        || location_lower.ends_with(", cn")
        || location_lower == "cn"
    {
        return "\u{1F1E8}\u{1F1F3}"; // CN flag
    }

    // India
    if location_lower.contains("india") || location_lower.ends_with(", in") {
        return "\u{1F1EE}\u{1F1F3}"; // IN flag
    }

    // El Salvador
    if location_lower.contains("el salvador")
        || location_lower.ends_with(", sv")
        || location_lower == "sv"
    {
        return "\u{1F1F8}\u{1F1FB}"; // SV flag
    }

    // Costa Rica
    if location_lower.contains("costa rica")
        || location_lower.ends_with(", cr")
        || location_lower == "cr"
    {
        return "\u{1F1E8}\u{1F1F7}"; // CR flag
    }

    // Thailand
    if location_lower.contains("thailand")
        || location_lower.ends_with(", th")
        || location_lower == "th"
    {
        return "\u{1F1F9}\u{1F1ED}"; // TH flag
    }

    // Vietnam
    if location_lower.contains("vietnam")
        || location_lower.ends_with(", vn")
        || location_lower == "vn"
    {
        return "\u{1F1FB}\u{1F1F3}"; // VN flag
    }

    // Indonesia
    if location_lower.contains("indonesia")
        || location_lower.ends_with(", id")
        || location_lower == "id"
    {
        return "\u{1F1EE}\u{1F1E9}"; // ID flag
    }

    // Malaysia
    if location_lower.contains("malaysia")
        || location_lower.ends_with(", my")
        || location_lower == "my"
    {
        return "\u{1F1F2}\u{1F1FE}"; // MY flag
    }

    // Philippines
    if location_lower.contains("philippines")
        || location_lower.ends_with(", ph")
        || location_lower == "ph"
    {
        return "\u{1F1F5}\u{1F1ED}"; // PH flag
    }

    // UAE
    if location_lower.contains("united arab emirates")
        || location_lower.contains("uae")
        || location_lower.ends_with(", ae")
        || location_lower == "ae"
    {
        return "\u{1F1E6}\u{1F1EA}"; // AE flag
    }

    // Israel
    if location_lower.contains("israel")
        || location_lower.ends_with(", il")
        || location_lower == "il"
    {
        return "\u{1F1EE}\u{1F1F1}"; // IL flag
    }

    // South Africa
    if location_lower.contains("south africa")
        || location_lower.ends_with(", za")
        || location_lower == "za"
    {
        return "\u{1F1FF}\u{1F1E6}"; // ZA flag
    }

    // Nigeria
    if location_lower.contains("nigeria")
        || location_lower.ends_with(", ng")
        || location_lower == "ng"
    {
        return "\u{1F1F3}\u{1F1EC}"; // NG flag
    }

    // Turkey
    if location_lower.contains("turkey")
        || location_lower.ends_with(", tr")
        || location_lower == "tr"
    {
        return "\u{1F1F9}\u{1F1F7}"; // TR flag
    }

    // Greece
    if location_lower.contains("greece")
        || location_lower.ends_with(", gr")
        || location_lower == "gr"
    {
        return "\u{1F1EC}\u{1F1F7}"; // GR flag
    }

    // Russia
    if location_lower.contains("russia")
        || location_lower.ends_with(", ru")
        || location_lower == "ru"
    {
        return "\u{1F1F7}\u{1F1FA}"; // RU flag
    }

    // Ukraine
    if location_lower.contains("ukraine")
        || location_lower.ends_with(", ua")
        || location_lower == "ua"
    {
        return "\u{1F1FA}\u{1F1E6}"; // UA flag
    }

    // Romania
    if location_lower.contains("romania")
        || location_lower.ends_with(", ro")
        || location_lower == "ro"
    {
        return "\u{1F1F7}\u{1F1F4}"; // RO flag
    }

    // Hungary
    if location_lower.contains("hungary")
        || location_lower.ends_with(", hu")
        || location_lower == "hu"
    {
        return "\u{1F1ED}\u{1F1FA}"; // HU flag
    }

    // Colombia
    if location_lower.contains("colombia")
        || location_lower.ends_with(", co")
        || location_lower == "co"
    {
        return "\u{1F1E8}\u{1F1F4}"; // CO flag
    }

    // Chile
    if location_lower.contains("chile")
        || location_lower.ends_with(", cl")
        || location_lower == "cl"
    {
        return "\u{1F1E8}\u{1F1F1}"; // CL flag
    }

    // Peru
    if location_lower.contains("peru") || location_lower.ends_with(", pe") || location_lower == "pe"
    {
        return "\u{1F1F5}\u{1F1EA}"; // PE flag
    }

    // No country detected
    ""
}

/// Get the flag emoji for a CachedEvent based on its locations.
/// Returns the flag from the first location that has a detectable country.
///
/// NOTE: Currently disabled - egui doesn't support flag emoji rendering.
/// Flag emojis are composed of regional indicator symbols which render as
/// separate letters (e.g., "U S" instead of 🇺🇸). Re-enable when egui adds
/// emoji font support. See bead notedeck_calendar-emj.
#[allow(dead_code)]
fn get_event_flag(_event: &CachedEvent) -> &'static str {
    // Disabled until egui supports flag emojis
    ""
    /* Original implementation:
    if event.locations.is_empty() {
        return "";
    }
    for location in &event.locations {
        let flag = country_flag_from_location(location);
        if !flag.is_empty() {
            return flag;
        }
    }
    ""
    */
}

/// View mode for the calendar display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CalendarViewMode {
    /// Month grid view showing all days.
    #[default]
    Month,
    /// Week view with time slots.
    Week,
    /// Single day view with detailed time slots.
    Day,
    /// Agenda/list view of upcoming events.
    Agenda,
}

/// Saved navigation state for back navigation.
#[derive(Debug, Clone)]
struct NavigationState {
    /// The view mode at this state.
    view_mode: CalendarViewMode,
    /// The selected date at this state.
    selected_date: NaiveDate,
    /// The view date at this state.
    view_date: NaiveDate,
    /// Whether detail panel was showing.
    show_detail_panel: bool,
    /// Selected event index if any.
    selected_event: Option<usize>,
}

/// Cached calendar event for display.
#[derive(Debug, Clone)]
pub struct CachedEvent {
    /// The note key in nostrdb (used for fetching full event details).
    #[allow(dead_code)]
    pub note_key: NoteKey,
    /// Event title.
    pub title: String,
    /// Brief summary of the event.
    pub summary: Option<String>,
    /// Full description.
    pub content: String,
    /// Start date (for date-based events) or timestamp date.
    pub start_date: NaiveDate,
    /// Start time info for time-based events.
    pub start_time: Option<EventTime>,
    /// End time info for time-based events.
    pub end_time: Option<EventTime>,
    /// Event locations.
    pub locations: Vec<String>,
    /// Event kind (31922 or 31923).
    pub kind: u32,
    /// Author pubkey.
    pub pubkey: [u8; 32],
    /// Optional image URL for the event.
    pub image: Option<String>,
    /// Geohash for location coordinates (NIP-52 "g" tag).
    pub geohash: Option<String>,
    /// The d-tag identifier for addressable events (used for computing coordinates).
    #[allow(dead_code)]
    pub d_tag: String,
    /// NIP-33 event coordinates (kind:pubkey:d-tag).
    pub coordinates: String,
}

/// Time information for time-based events.
#[derive(Debug, Clone)]
pub struct EventTime {
    /// Unix timestamp.
    pub timestamp: u64,
    /// IANA timezone identifier.
    pub timezone: Option<String>,
}

/// Nostr Calendar App - View and browse NIP-52 calendar events.
///
/// This app provides a calendar interface for viewing calendar events
/// published on Nostr relays following the NIP-52 specification.
pub struct CalendarApp {
    /// Current view mode (month, week, day, agenda).
    view_mode: CalendarViewMode,
    /// Currently selected date.
    selected_date: NaiveDate,
    /// Date being viewed (for navigation).
    view_date: NaiveDate,
    /// Local nostrdb subscription for calendar events.
    subscription: Option<Subscription>,
    /// Remote relay subscription ID.
    remote_sub_id: Option<String>,
    /// Set of relay URLs we've already subscribed to.
    subscribed_relays: HashSet<String>,
    /// Cached events for display.
    events: Vec<CachedEvent>,
    /// Whether we need to refresh events from nostrdb.
    needs_refresh: bool,
    /// Currently selected event for detail view (index into events).
    selected_event: Option<usize>,
    /// Whether to show the event detail panel.
    show_detail_panel: bool,
    /// Navigation history stack for back navigation.
    nav_history: Vec<NavigationState>,
    /// Swipe tracking: start position for gesture detection.
    swipe_start: Option<egui::Pos2>,
    /// Search query for filtering events.
    search_query: String,
    /// Whether to show only events from followed users (web-of-trust).
    filter_wot: bool,
    /// Country filter (None = all countries).
    filter_country: Option<String>,
    /// Local nostrdb subscription for comments (kind 1111).
    comments_subscription: Option<Subscription>,
    /// Remote relay subscription ID for comments.
    comments_remote_sub_id: Option<String>,
    /// Cached comments indexed by event coordinates.
    comments: std::collections::HashMap<String, Vec<CachedComment>>,
    /// Whether we need to refresh comments from nostrdb.
    comments_needs_refresh: bool,
    /// Whether the comments section is expanded in the detail view.
    comments_expanded: bool,
}

impl Default for CalendarApp {
    fn default() -> Self {
        Self::new()
    }
}

impl CalendarApp {
    /// Create a new calendar app with default settings.
    pub fn new() -> Self {
        let today = Local::now().date_naive();
        Self {
            view_mode: CalendarViewMode::Month,
            selected_date: today,
            view_date: today,
            subscription: None,
            remote_sub_id: None,
            subscribed_relays: HashSet::new(),
            events: Vec::new(),
            needs_refresh: true,
            selected_event: None,
            show_detail_panel: false,
            nav_history: Vec::new(),
            swipe_start: None,
            search_query: String::new(),
            filter_wot: false,
            filter_country: None,
            comments_subscription: None,
            comments_remote_sub_id: None,
            comments: std::collections::HashMap::new(),
            comments_needs_refresh: true,
            comments_expanded: false,
        }
    }

    /// Check if an event matches the current filters.
    ///
    /// The `is_following` closure is used for web-of-trust filtering - it should
    /// return the following status for a given pubkey.
    fn event_matches_filters<F>(&self, event: &CachedEvent, is_following: F) -> bool
    where
        F: Fn(&[u8; 32]) -> IsFollowing,
    {
        // Search query filter
        if !self.search_query.is_empty() {
            let query_lower = self.search_query.to_lowercase();
            let title_match = event.title.to_lowercase().contains(&query_lower);
            let content_match = event.content.to_lowercase().contains(&query_lower);
            let location_match = event
                .locations
                .iter()
                .any(|loc| loc.to_lowercase().contains(&query_lower));
            if !title_match && !content_match && !location_match {
                return false;
            }
        }

        // Web-of-trust filter - only show events from followed users
        if self.filter_wot {
            match is_following(&event.pubkey) {
                IsFollowing::Yes => {} // Passes filter
                IsFollowing::No | IsFollowing::Unknown => return false,
            }
        }

        // Country filter
        if let Some(ref country) = self.filter_country {
            let country_lower = country.to_lowercase();
            let has_country = event
                .locations
                .iter()
                .any(|loc| loc.to_lowercase().contains(&country_lower));
            if !has_country {
                return false;
            }
        }

        true
    }

    /// Get list of unique countries from all events.
    fn get_available_countries(&self) -> Vec<String> {
        let mut countries: HashSet<String> = HashSet::new();

        for event in &self.events {
            for location in &event.locations {
                // Extract country using the same logic as flag detection
                let loc_lower = location.to_lowercase();
                if loc_lower.contains("united states")
                    || loc_lower.contains("usa")
                    || loc_lower.ends_with(", us")
                {
                    countries.insert("United States".to_string());
                } else if loc_lower.contains("canada") {
                    countries.insert("Canada".to_string());
                } else if loc_lower.contains("germany") || loc_lower.contains("deutschland") {
                    countries.insert("Germany".to_string());
                } else if loc_lower.contains("united kingdom")
                    || loc_lower.contains("england")
                    || loc_lower.ends_with(", uk")
                {
                    countries.insert("United Kingdom".to_string());
                } else if loc_lower.contains("japan") {
                    countries.insert("Japan".to_string());
                } else if loc_lower.contains("france") {
                    countries.insert("France".to_string());
                } else if loc_lower.contains("spain") {
                    countries.insert("Spain".to_string());
                } else if loc_lower.contains("italy") || loc_lower.contains("italia") {
                    countries.insert("Italy".to_string());
                } else if loc_lower.contains("netherlands") || loc_lower.contains("holland") {
                    countries.insert("Netherlands".to_string());
                } else if loc_lower.contains("australia") {
                    countries.insert("Australia".to_string());
                } else if loc_lower.contains("brazil") || loc_lower.contains("brasil") {
                    countries.insert("Brazil".to_string());
                } else if loc_lower.contains("mexico") {
                    countries.insert("Mexico".to_string());
                } else if loc_lower.contains("switzerland") || loc_lower.contains("schweiz") {
                    countries.insert("Switzerland".to_string());
                } else if loc_lower.contains("thailand") {
                    countries.insert("Thailand".to_string());
                } else if loc_lower.contains("sweden") {
                    countries.insert("Sweden".to_string());
                } else if loc_lower.contains("greece") {
                    countries.insert("Greece".to_string());
                } else if loc_lower.contains("belgium") {
                    countries.insert("Belgium".to_string());
                } else if loc_lower.contains("peru") {
                    countries.insert("Peru".to_string());
                }
                // Add more countries as needed
            }
        }

        let mut result: Vec<String> = countries.into_iter().collect();
        result.sort();
        result
    }

    /// Push current state to navigation history before changing view.
    fn push_nav_state(&mut self) {
        self.nav_history.push(NavigationState {
            view_mode: self.view_mode,
            selected_date: self.selected_date,
            view_date: self.view_date,
            show_detail_panel: self.show_detail_panel,
            selected_event: self.selected_event,
        });
    }

    /// Pop and restore previous navigation state. Returns true if navigated back.
    fn navigate_back(&mut self) -> bool {
        if let Some(state) = self.nav_history.pop() {
            self.view_mode = state.view_mode;
            self.selected_date = state.selected_date;
            self.view_date = state.view_date;
            self.show_detail_panel = state.show_detail_panel;
            self.selected_event = state.selected_event;
            true
        } else {
            false
        }
    }

    /// Check if back navigation is available.
    fn can_go_back(&self) -> bool {
        !self.nav_history.is_empty()
    }

    /// Navigate to a specific date in day view (drill-down).
    fn drill_down_to_day(&mut self, date: NaiveDate) {
        self.push_nav_state();
        self.view_mode = CalendarViewMode::Day;
        self.selected_date = date;
        self.view_date = date;
    }

    /// Handle swipe gesture for back navigation.
    /// Returns true if a back navigation was triggered.
    fn handle_swipe(&mut self, ui: &egui::Ui) -> bool {
        let response = ui.interact(
            ui.available_rect_before_wrap(),
            ui.id().with("swipe_area"),
            egui::Sense::drag(),
        );

        if response.drag_started() {
            self.swipe_start = response.interact_pointer_pos();
        }

        if response.drag_stopped() {
            if let Some(start) = self.swipe_start.take() {
                if let Some(end) = response.interact_pointer_pos() {
                    let delta = end - start;
                    // Swipe right to go back (horizontal > 100px, mostly horizontal)
                    if delta.x > 100.0 && delta.x.abs() > delta.y.abs() * 2.0 {
                        return self.navigate_back();
                    }
                }
            }
        }

        false
    }

    /// Get the current view mode.
    pub fn view_mode(&self) -> CalendarViewMode {
        self.view_mode
    }

    /// Set the view mode.
    pub fn set_view_mode(&mut self, mode: CalendarViewMode) {
        self.view_mode = mode;
    }

    /// Get the selected date.
    pub fn selected_date(&self) -> NaiveDate {
        self.selected_date
    }

    /// Set the selected date.
    pub fn set_selected_date(&mut self, date: NaiveDate) {
        self.selected_date = date;
    }

    /// Navigate to the previous period (month/week/day depending on view).
    pub fn navigate_previous(&mut self) {
        self.view_date = match self.view_mode {
            CalendarViewMode::Month => self
                .view_date
                .checked_sub_months(chrono::Months::new(1))
                .unwrap_or(self.view_date),
            CalendarViewMode::Week => self
                .view_date
                .checked_sub_days(chrono::Days::new(7))
                .unwrap_or(self.view_date),
            CalendarViewMode::Day | CalendarViewMode::Agenda => self
                .view_date
                .checked_sub_days(chrono::Days::new(1))
                .unwrap_or(self.view_date),
        };
        self.needs_refresh = true;
    }

    /// Navigate to the next period (month/week/day depending on view).
    pub fn navigate_next(&mut self) {
        self.view_date = match self.view_mode {
            CalendarViewMode::Month => self
                .view_date
                .checked_add_months(chrono::Months::new(1))
                .unwrap_or(self.view_date),
            CalendarViewMode::Week => self
                .view_date
                .checked_add_days(chrono::Days::new(7))
                .unwrap_or(self.view_date),
            CalendarViewMode::Day | CalendarViewMode::Agenda => self
                .view_date
                .checked_add_days(chrono::Days::new(1))
                .unwrap_or(self.view_date),
        };
        self.needs_refresh = true;
    }

    /// Navigate to today.
    pub fn navigate_today(&mut self) {
        let today = Local::now().date_naive();
        self.view_date = today;
        self.selected_date = today;
        self.needs_refresh = true;
    }

    /// Initialize subscription for calendar events.
    fn ensure_subscription(&mut self, ctx: &mut AppContext<'_>) {
        // Create local nostrdb subscription if not already done
        if self.subscription.is_none() {
            let filter = calendar_events_filter(500);
            match ctx.ndb.subscribe(std::slice::from_ref(&filter)) {
                Ok(sub) => {
                    self.subscription = Some(sub);
                    tracing::info!("Calendar: Local nostrdb subscription created");
                }
                Err(e) => {
                    tracing::error!("Calendar: Failed to create local subscription: {}", e);
                    return;
                }
            }

            // Generate subscription ID for remote relays
            let subid = Uuid::new_v4().to_string();
            self.remote_sub_id = Some(subid.clone());
            tracing::info!("Calendar: Generated relay subscription ID: {}", subid);

            // Log the filter for debugging
            match filter.json() {
                Ok(filter_json) => tracing::info!("Calendar: Filter JSON: {}", filter_json),
                Err(e) => tracing::error!("Calendar: Failed to serialize filter: {:?}", e),
            }
        }

        // Send subscription to any connected relays we haven't subscribed to yet
        let Some(subid) = &self.remote_sub_id else {
            return;
        };
        let filter = calendar_events_filter(500);

        for relay in &mut ctx.pool.relays {
            let relay_url = relay.url().to_string();

            // Skip if we've already subscribed to this relay
            if self.subscribed_relays.contains(&relay_url) {
                continue;
            }

            // Only send to connected relays
            let is_connected = matches!(relay.status(), notedeck::enostr::RelayStatus::Connected);
            if !is_connected {
                tracing::trace!(
                    "Calendar: Relay {} not connected (status: {:?}), skipping",
                    relay_url,
                    relay.status()
                );
                continue;
            }

            // Send subscription to this relay
            let msg = ClientMessage::req(subid.clone(), vec![filter.clone()]);
            if let Err(e) = relay.send(&msg) {
                tracing::error!(
                    "Calendar: Failed to send subscription to {}: {}",
                    relay_url,
                    e
                );
            } else {
                tracing::info!("Calendar: Sent subscription to relay {}", relay_url);
                self.subscribed_relays.insert(relay_url);
            }
        }
    }

    /// Poll for new events and refresh cache.
    fn poll_events(&mut self, ctx: &mut AppContext<'_>) {
        let Some(sub) = self.subscription else {
            return;
        };

        // Poll for new note keys
        let new_keys = ctx.ndb.poll_for_notes(sub, 500);
        if !new_keys.is_empty() {
            self.needs_refresh = true;
            tracing::info!(
                "Calendar: poll_for_notes returned {} new note keys!",
                new_keys.len()
            );
        }
    }

    /// Initialize subscription for comments (kind 1111).
    fn ensure_comments_subscription(&mut self, ctx: &mut AppContext<'_>) {
        // Create local nostrdb subscription for comments if not already done
        if self.comments_subscription.is_none() {
            let filter = calendar_comments_filter(200);
            match ctx.ndb.subscribe(std::slice::from_ref(&filter)) {
                Ok(sub) => {
                    self.comments_subscription = Some(sub);
                    tracing::info!("Calendar: Comments subscription created");
                }
                Err(e) => {
                    tracing::error!("Calendar: Failed to create comments subscription: {}", e);
                    return;
                }
            }

            // Generate subscription ID for remote relays
            let subid = Uuid::new_v4().to_string();
            self.comments_remote_sub_id = Some(subid.clone());
            tracing::info!(
                "Calendar: Generated comments relay subscription ID: {}",
                subid
            );
        }

        // Send subscription to any connected relays we haven't subscribed to yet
        let Some(subid) = &self.comments_remote_sub_id else {
            return;
        };
        let filter = calendar_comments_filter(200);

        for relay in &mut ctx.pool.relays {
            let relay_url = relay.url().to_string();

            // Skip if we've already subscribed to this relay
            if self.subscribed_relays.contains(&relay_url) {
                continue;
            }

            // Only send to connected relays
            let is_connected = matches!(relay.status(), notedeck::enostr::RelayStatus::Connected);
            if !is_connected {
                continue;
            }

            // Send comments subscription to this relay
            let msg = ClientMessage::req(subid.clone(), vec![filter.clone()]);
            if let Err(e) = relay.send(&msg) {
                tracing::error!(
                    "Calendar: Failed to send comments subscription to {}: {}",
                    relay_url,
                    e
                );
            } else {
                tracing::info!(
                    "Calendar: Sent comments subscription to relay {}",
                    relay_url
                );
            }
        }
    }

    /// Poll for new comments.
    fn poll_comments(&mut self, ctx: &mut AppContext<'_>) {
        let Some(sub) = self.comments_subscription else {
            return;
        };

        // Poll for new comment note keys
        let new_keys = ctx.ndb.poll_for_notes(sub, 200);
        if !new_keys.is_empty() {
            self.comments_needs_refresh = true;
            tracing::info!(
                "Calendar: poll_for_notes returned {} new comment keys!",
                new_keys.len()
            );
        }
    }

    /// Refresh the comments cache from nostrdb.
    fn refresh_comments(&mut self, ctx: &mut AppContext<'_>) {
        if !self.comments_needs_refresh {
            return;
        }
        self.comments_needs_refresh = false;

        let Ok(txn) = Transaction::new(ctx.ndb) else {
            tracing::error!("Failed to create transaction for comments");
            return;
        };

        // Query comments
        let filter = calendar_comments_filter(200);
        let results = match ctx.ndb.query(&txn, &[filter], 200) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to query comments: {}", e);
                return;
            }
        };

        if !results.is_empty() {
            tracing::info!("Calendar: Found {} comments", results.len());
        }

        // Clear and rebuild comments map
        self.comments.clear();

        for result in results {
            let Ok(note) = ctx.ndb.get_note_by_key(&txn, result.note_key) else {
                continue;
            };

            if note.kind() != KIND_COMMENT {
                continue;
            }

            let Some(comment) = Comment::from_note(&note, result.note_key) else {
                continue;
            };

            // Request profile fetch for comment author if not cached
            ctx.unknown_ids
                .add_pubkey_if_missing(ctx.ndb, &txn, &comment.pubkey);

            // Index by root A tag (event coordinates)
            if let Some(root_a) = comment.root_a_tag.clone() {
                // Get author display name
                let author_name =
                    if let Ok(profile) = ctx.ndb.get_profile_by_pubkey(&txn, &comment.pubkey) {
                        let nostr_name = notedeck::name::get_display_name(Some(&profile));
                        let name = nostr_name.name();
                        if name != "??" {
                            name.to_string()
                        } else {
                            comment.author_short()
                        }
                    } else {
                        comment.author_short()
                    };

                let mut cached = CachedComment::new(comment);
                cached.set_author_name(author_name);

                self.comments.entry(root_a).or_default().push(cached);
            }
        }

        // Sort comments by created_at (newest first)
        for comments in self.comments.values_mut() {
            comments.sort_by(|a, b| b.comment.created_at.cmp(&a.comment.created_at));
        }

        let total_comments: usize = self.comments.values().map(|v| v.len()).sum();
        let events_with_comments = self.comments.len();
        tracing::info!(
            "Calendar: Indexed {} comments across {} events",
            total_comments,
            events_with_comments
        );
    }

    /// Get the number of comments for an event.
    fn get_comment_count(&self, event_coordinates: &str) -> usize {
        self.comments.get(event_coordinates).map_or(0, |c| c.len())
    }

    /// Get comments for an event.
    fn get_comments(&self, event_coordinates: &str) -> Option<&Vec<CachedComment>> {
        self.comments.get(event_coordinates)
    }

    /// Refresh the event cache from nostrdb.
    fn refresh_events(&mut self, ctx: &mut AppContext<'_>) {
        if !self.needs_refresh {
            return;
        }
        self.needs_refresh = false;

        let Ok(txn) = Transaction::new(ctx.ndb) else {
            tracing::error!("Failed to create transaction");
            return;
        };

        // Query calendar events
        let filter = calendar_events_filter(500);
        let results = match ctx.ndb.query(&txn, &[filter], 500) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to query calendar events: {}", e);
                return;
            }
        };

        if results.is_empty() {
            // Debug: try querying without the subscription to see what's in ndb
            tracing::warn!(
                "Calendar: Query returned 0 results. Relays may not have NIP-52 events, or events haven't arrived yet."
            );
        } else {
            tracing::info!(
                "Calendar: Query returned {} results (looking for kinds 31922, 31923, 31924, 31925)",
                results.len()
            );
        }

        // Clear selected event when refreshing
        self.selected_event = None;
        self.events.clear();

        let mut parsed_count = 0;
        let mut skipped_kind = 0;
        let mut failed_parse = 0;

        for result in results {
            let Ok(note) = ctx.ndb.get_note_by_key(&txn, result.note_key) else {
                continue;
            };

            let kind = note.kind();
            if kind != KIND_DATE_CALENDAR_EVENT && kind != KIND_TIME_CALENDAR_EVENT {
                skipped_kind += 1;
                continue;
            }

            // Parse the event to get title and date
            let Some(event) = CalendarEvent::from_note(&note) else {
                failed_parse += 1;
                continue;
            };

            // Request profile fetch for author if not cached
            ctx.unknown_ids
                .add_pubkey_if_missing(ctx.ndb, &txn, &event.pubkey);

            // Extract start date and time info
            let (start_date, start_time) = match &event.start {
                CalendarTime::Date(date) => (*date, None),
                CalendarTime::Timestamp {
                    timestamp,
                    timezone,
                } => {
                    let dt = Local.timestamp_opt(*timestamp as i64, 0);
                    match dt {
                        chrono::LocalResult::Single(dt) => (
                            dt.date_naive(),
                            Some(EventTime {
                                timestamp: *timestamp,
                                timezone: timezone.clone(),
                            }),
                        ),
                        _ => continue,
                    }
                }
            };

            // Extract end time info
            let end_time = event.end.as_ref().and_then(|end| match end {
                CalendarTime::Timestamp {
                    timestamp,
                    timezone,
                } => Some(EventTime {
                    timestamp: *timestamp,
                    timezone: timezone.clone(),
                }),
                CalendarTime::Date(_) => None,
            });

            // Build event coordinates for comment matching
            let coordinates = event.coordinates();

            parsed_count += 1;
            self.events.push(CachedEvent {
                note_key: result.note_key,
                title: event.title,
                summary: event.summary,
                content: event.content,
                start_date,
                start_time,
                end_time,
                locations: event.locations,
                kind,
                pubkey: event.pubkey,
                image: event.image,
                geohash: event.geohash,
                d_tag: event.d_tag,
                coordinates,
            });
        }

        // Sort events by date
        self.events.sort_by_key(|e| e.start_date);

        // Count events with location data
        let with_location = self
            .events
            .iter()
            .filter(|e| !e.locations.is_empty())
            .count();
        let with_geohash = self.events.iter().filter(|e| e.geohash.is_some()).count();

        tracing::info!(
            "Calendar: Parsed {} events, skipped {} (wrong kind), failed to parse {}. {} have locations, {} have geohash.",
            parsed_count,
            skipped_kind,
            failed_parse,
            with_location,
            with_geohash
        );
    }

    /// Render the calendar header with navigation.
    fn render_header(&mut self, ui: &mut Ui) {
        // Touch-friendly minimum button size (44x44pt per Apple HIG)
        let min_button_size = Vec2::new(44.0, 44.0);
        let can_go_back = self.can_go_back();
        let mut go_back = false;

        ui.horizontal(|ui| {
            ui.spacing_mut().button_padding = Vec2::new(12.0, 6.0);

            // Back button (only shown when history exists)
            if can_go_back {
                if ui
                    .add_sized(min_button_size, egui::Button::new("\u{2190}")) // ← arrow
                    .on_hover_text("Go back")
                    .clicked()
                {
                    go_back = true;
                }
                ui.separator();
            }

            if ui
                .add_sized(min_button_size, egui::Button::new("<"))
                .on_hover_text("Previous")
                .clicked()
            {
                self.navigate_previous();
            }

            if ui
                .add_sized(Vec2::new(60.0, 32.0), egui::Button::new("Today"))
                .clicked()
            {
                self.navigate_today();
            }

            if ui
                .add_sized(min_button_size, egui::Button::new(">"))
                .on_hover_text("Next")
                .clicked()
            {
                self.navigate_next();
            }

            ui.add_space(8.0);

            // Month and year display
            let month_year = self.view_date.format("%B %Y").to_string();
            ui.heading(&month_year);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // View mode selector with spacing
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.selectable_value(&mut self.view_mode, CalendarViewMode::Agenda, "Agenda");
                ui.selectable_value(&mut self.view_mode, CalendarViewMode::Day, "Day");
                ui.selectable_value(&mut self.view_mode, CalendarViewMode::Week, "Week");
                ui.selectable_value(&mut self.view_mode, CalendarViewMode::Month, "Month");
            });
        });

        // Apply back navigation after UI rendering
        if go_back {
            self.navigate_back();
        }

        // Filter bar
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            // Search box
            ui.label("Search:");
            let search_response = ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .desired_width(150.0)
                    .hint_text("Filter events..."),
            );
            if search_response.changed() {
                // Search updated
            }

            // Clear search button
            if !self.search_query.is_empty() && ui.small_button("×").clicked() {
                self.search_query.clear();
            }

            ui.separator();

            // WoT filter toggle
            if ui
                .selectable_label(self.filter_wot, "Following Only")
                .clicked()
            {
                self.filter_wot = !self.filter_wot;
            }

            ui.separator();

            // Country filter dropdown
            egui::ComboBox::from_label("")
                .selected_text(self.filter_country.as_deref().unwrap_or("All Countries"))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.filter_country.is_none(), "All Countries")
                        .clicked()
                    {
                        self.filter_country = None;
                    }
                    let countries = self.get_available_countries();
                    for country in countries {
                        let is_selected = self.filter_country.as_ref() == Some(&country);
                        if ui.selectable_label(is_selected, &country).clicked() {
                            self.filter_country = Some(country);
                        }
                    }
                });

            // Show active filter count
            let active_filters = [
                !self.search_query.is_empty(),
                self.filter_wot,
                self.filter_country.is_some(),
            ]
            .iter()
            .filter(|&&x| x)
            .count();

            if active_filters > 0 {
                ui.separator();
                ui.label(RichText::new(format!("{} filter(s) active", active_filters)).weak());
                if ui.small_button("Clear All").clicked() {
                    self.search_query.clear();
                    self.filter_wot = false;
                    self.filter_country = None;
                }
            }
        });
    }

    /// Select an event by its index and show the detail panel.
    fn select_event(&mut self, index: usize) {
        self.push_nav_state();
        self.selected_event = Some(index);
        self.show_detail_panel = true;
    }

    /// Close the event detail panel (uses back navigation if available).
    fn close_detail_panel(&mut self) {
        if !self.navigate_back() {
            // Fallback if no history
            self.show_detail_panel = false;
            self.selected_event = None;
        }
    }

    /// Render the event detail panel.
    fn render_event_detail(
        &mut self,
        ui: &mut Ui,
        ndb: &Ndb,
        img_cache: &mut Images,
        media_jobs: &MediaJobs,
    ) {
        let Some(idx) = self.selected_event else {
            return;
        };
        let Some(event) = self.events.get(idx).cloned() else {
            self.selected_event = None;
            return;
        };

        // Color accent for event type
        let accent_color = if event.kind == KIND_DATE_CALENDAR_EVENT {
            Color32::from_rgb(76, 175, 80) // Green for all-day
        } else {
            Color32::from_rgb(100, 140, 230) // Blue for timed
        };

        // Close button (top right, 44x44pt touch target per Apple HIG)
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if ui
                .add_sized(
                    Vec2::new(44.0, 44.0),
                    egui::Button::new(RichText::new("✕").size(18.0)).frame(false),
                )
                .on_hover_text("Close")
                .clicked()
            {
                self.close_detail_panel();
            }
        });

        // Title - large and prominent
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            // Color bar indicator
            let (rect, _) = ui.allocate_exact_size(Vec2::new(4.0, 28.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(2), accent_color);
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(&event.title).size(20.0).strong()).wrap());
        });

        // Event image (if available)
        if let Some(ref image_url) = event.image {
            ui.add_space(12.0);
            let max_image_height = 200.0;
            let available_width = ui.available_width();

            // Try to load the image texture
            if let Some(texture) = img_cache.latest_texture(
                media_jobs.sender(),
                ui,
                image_url,
                ImageType::Content(None),
                AnimationMode::NoAnimation,
            ) {
                // Calculate size while maintaining aspect ratio
                let tex_size = texture.size_vec2();
                let aspect = tex_size.x / tex_size.y;
                let display_width = available_width.min(tex_size.x);
                let display_height = (display_width / aspect).min(max_image_height);
                let final_width = display_height * aspect;

                ui.add(
                    egui::Image::new(texture)
                        .fit_to_exact_size(Vec2::new(final_width, display_height))
                        .corner_radius(CornerRadius::same(8)),
                );
            } else {
                // Show loading placeholder
                let placeholder_height = 120.0;
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(available_width, placeholder_height),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(8), Color32::from_gray(60));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Loading image...",
                    egui::FontId::default(),
                    Color32::from_gray(140),
                );
            }
        }

        ui.add_space(16.0);

        // Date & Time section - clean layout
        ui.horizontal(|ui| {
            ui.label(RichText::new("📅").size(16.0));
            ui.add_space(8.0);
            ui.vertical(|ui| {
                // Date
                ui.label(
                    RichText::new(event.start_date.format("%A, %B %d, %Y").to_string()).size(14.0),
                );
                // Time (if timed event)
                if let Some(ref start_time) = event.start_time {
                    let time_str =
                        format_time(start_time.timestamp, start_time.timezone.as_deref());
                    let time_display = if let Some(ref end_time) = event.end_time {
                        let end_str = format_time(end_time.timestamp, end_time.timezone.as_deref());
                        format!("{} – {}", time_str, end_str)
                    } else {
                        time_str
                    };
                    ui.label(RichText::new(time_display).size(14.0));
                } else {
                    ui.label(RichText::new("All day").size(14.0));
                }
            });
        });

        // Location section with country flag
        if !event.locations.is_empty() {
            ui.add_space(12.0);
            let flag = get_event_flag(&event);
            ui.horizontal(|ui| {
                // Show flag instead of pin emoji if we have a country flag
                if !flag.is_empty() {
                    ui.label(RichText::new(flag).size(16.0));
                } else {
                    ui.label(RichText::new("\u{1F4CD}").size(16.0)); // pin emoji
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    for loc in &event.locations {
                        ui.label(RichText::new(loc).size(14.0));
                    }
                });
            });

            // Show static map if geohash is available and valid
            if let Some(ref geohash_str) = event.geohash {
                // Validate geohash before decoding (geohash library can panic on invalid input)
                let is_valid_geohash = !geohash_str.is_empty()
                    && geohash_str.len() <= 12
                    && geohash_str.chars().all(|c| {
                        // Valid geohash base32 alphabet: 0-9, b-h, j-k, m-n, p-z (excludes a, i, l, o)
                        matches!(c, '0'..='9' | 'b'..='h' | 'j'..='k' | 'm'..='n' | 'p'..='z')
                    });

                if is_valid_geohash {
                    if let Ok((coord, _, _)) = geohash::decode(geohash_str) {
                        ui.add_space(12.0);

                        // Convert lat/lon to OSM tile coordinates (zoom level 14)
                        let zoom = 14u32;
                        let lat_rad = coord.y.to_radians();
                        let n = 2.0_f64.powi(zoom as i32);
                        let tile_x = ((coord.x + 180.0) / 360.0 * n).floor() as u32;
                        let tile_y = ((1.0 - lat_rad.tan().asinh() / std::f64::consts::PI) / 2.0
                            * n)
                            .floor() as u32;

                        // OSM static tile URL
                        let tile_url = format!(
                            "https://tile.openstreetmap.org/{}/{}/{}.png",
                            zoom, tile_x, tile_y
                        );

                        // Load and display the map tile using notedeck's image system
                        let map_size = 150.0;
                        if let Some(texture) = img_cache.latest_texture(
                            media_jobs.sender(),
                            ui,
                            &tile_url,
                            ImageType::Content(None),
                            AnimationMode::NoAnimation,
                        ) {
                            ui.add(
                                egui::Image::new(texture)
                                    .fit_to_exact_size(Vec2::new(map_size, map_size))
                                    .corner_radius(CornerRadius::same(8)),
                            );
                        } else {
                            // Show loading placeholder
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new(map_size, map_size),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(
                                rect,
                                CornerRadius::same(8),
                                Color32::from_gray(60),
                            );
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Loading map...",
                                egui::FontId::default(),
                                Color32::from_gray(140),
                            );
                        }

                        // Add "View on OpenStreetMap" link
                        let osm_url = format!(
                            "https://www.openstreetmap.org/?mlat={}&mlon={}&zoom={}",
                            coord.y, coord.x, zoom
                        );
                        if ui.link("View on OpenStreetMap").clicked() {
                            if let Err(e) = opener::open(&osm_url) {
                                tracing::warn!("Failed to open OSM link: {}", e);
                            }
                        }
                    }
                } // close is_valid_geohash
            }
        }

        // Summary (if different from title)
        if let Some(ref summary) = event.summary {
            if summary != &event.title {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("📝").size(16.0));
                    ui.add_space(8.0);
                    ui.label(RichText::new(summary).size(14.0));
                });
            }
        }

        // Description section
        if !event.content.is_empty() {
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(12.0);

            ui.label(RichText::new("Details").size(14.0).strong());
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("event_description")
                .max_height(180.0)
                .show(ui, |ui| {
                    ui.label(RichText::new(&event.content).size(13.0));
                });
        }

        // Author info with profile picture
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        // Try to get profile name and picture
        let (author_name, profile_url) = if let Ok(txn) = Transaction::new(ndb) {
            let profile = ndb.get_profile_by_pubkey(&txn, &event.pubkey).ok();
            let nostr_name = get_display_name(profile.as_ref());
            let name = nostr_name.name();
            let pfp_url = get_profile_url(profile.as_ref()).to_string();
            let display_name = if name != "??" {
                name.to_string()
            } else {
                let pubkey_hex = hex::encode(event.pubkey);
                format!("{}...{}", &pubkey_hex[..8], &pubkey_hex[56..])
            };
            (display_name, pfp_url)
        } else {
            let pubkey_hex = hex::encode(event.pubkey);
            (
                format!("{}...{}", &pubkey_hex[..8], &pubkey_hex[56..]),
                "https://damus.io/img/no-profile.svg".to_string(),
            )
        };

        ui.horizontal(|ui| {
            // Profile picture (small circular avatar)
            let pfp_size = 28.0;
            if let Some(texture) = img_cache.latest_texture(
                media_jobs.sender(),
                ui,
                &profile_url,
                ImageType::Profile(pfp_size as u32),
                AnimationMode::NoAnimation,
            ) {
                ui.add(
                    egui::Image::new(texture)
                        .fit_to_exact_size(Vec2::splat(pfp_size))
                        .corner_radius(CornerRadius::same(14)), // half of 28
                );
            } else {
                // Placeholder circle while loading
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(pfp_size), egui::Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), pfp_size / 2.0, Color32::from_gray(80));
            }

            ui.add_space(8.0);
            ui.label(RichText::new(format!("Posted by {}", author_name)).size(13.0));
        });

        // Comments section
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        let comment_count = self.get_comment_count(&event.coordinates);
        let comments_header = if comment_count > 0 {
            format!("Comments ({})", comment_count)
        } else {
            "Comments".to_string()
        };

        // Collapsible header - make entire row clickable
        let arrow = if self.comments_expanded {
            "\u{25BC}"
        } else {
            "\u{25B6}"
        }; // down/right arrow

        let header_text = format!("{} {}", arrow, comments_header);
        let header_response = ui.add(
            egui::Label::new(RichText::new(header_text).size(14.0).strong())
                .sense(egui::Sense::click()),
        );

        if header_response.clicked() {
            self.comments_expanded = !self.comments_expanded;
        }

        // Show comments if expanded
        if self.comments_expanded {
            ui.add_space(8.0);

            if let Some(comments) = self.get_comments(&event.coordinates) {
                if comments.is_empty() {
                    ui.label(RichText::new("No comments yet").weak().italics());
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("event_comments")
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for cached_comment in comments {
                                let comment = &cached_comment.comment;

                                egui::Frame::new()
                                    .fill(Color32::from_gray(45))
                                    .inner_margin(8.0)
                                    .corner_radius(CornerRadius::same(6))
                                    .show(ui, |ui| {
                                        // Author and timestamp
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(&cached_comment.author_name)
                                                    .size(12.0)
                                                    .strong(),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    // Format timestamp
                                                    let dt = chrono::Local.timestamp_opt(
                                                        comment.created_at as i64,
                                                        0,
                                                    );
                                                    if let chrono::LocalResult::Single(dt) = dt {
                                                        let time_str = dt
                                                            .format("%b %d, %Y %H:%M")
                                                            .to_string();
                                                        ui.label(
                                                            RichText::new(time_str)
                                                                .size(10.0)
                                                                .weak(),
                                                        );
                                                    }
                                                },
                                            );
                                        });

                                        ui.add_space(4.0);

                                        // Comment content
                                        ui.label(RichText::new(&comment.content).size(13.0));

                                        // Reply indicator if this is a reply to another comment
                                        if !comment.is_root_comment() {
                                            ui.add_space(4.0);
                                            ui.label(
                                                RichText::new("(reply)")
                                                    .size(10.0)
                                                    .weak()
                                                    .italics(),
                                            );
                                        }
                                    });

                                ui.add_space(6.0);
                            }
                        });
                }
            } else {
                ui.label(RichText::new("No comments yet").weak().italics());
            }
        }
    }

    /// Render the main calendar content based on view mode.
    fn render_content<F>(&mut self, ui: &mut Ui, is_following: F)
    where
        F: Fn(&[u8; 32]) -> IsFollowing + Copy,
    {
        match self.view_mode {
            CalendarViewMode::Month => self.render_month_view(ui, is_following),
            CalendarViewMode::Week => self.render_week_view(ui, is_following),
            CalendarViewMode::Day => self.render_day_view(ui, is_following),
            CalendarViewMode::Agenda => self.render_agenda_view(ui, is_following),
        }
    }

    /// Render month grid view.
    fn render_month_view<F>(&mut self, ui: &mut Ui, is_following: F)
    where
        F: Fn(&[u8; 32]) -> IsFollowing + Copy,
    {
        let year = self.view_date.year();
        let month = self.view_date.month();

        // Event colors for visual distinction
        let date_event_color = Color32::from_rgb(76, 175, 80); // Green for all-day
        let time_event_color = Color32::from_rgb(33, 150, 243); // Blue for timed

        // Calculate dimensions based on available space
        let available_width = ui.available_width();
        let cell_width = (available_width / 7.0).max(80.0);
        let cell_height = 100.0;

        // Weekday headers - use full width
        egui::Grid::new("weekday_headers")
            .num_columns(7)
            .min_col_width(cell_width)
            .max_col_width(cell_width)
            .spacing([0.0, 0.0])
            .show(ui, |ui| {
                for day in ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(day).strong());
                    });
                }
                ui.end_row();
            });

        ui.separator();

        // Get first day of month and calculate grid start
        let first_of_month = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
        let first_weekday = first_of_month.weekday().num_days_from_sunday() as i32;
        let grid_start = first_of_month - chrono::Days::new(first_weekday as u64);

        // Pre-collect event info to avoid borrow conflicts
        let today = Local::now().date_naive();
        let mut new_selected: Option<NaiveDate> = None;
        let mut event_to_select: Option<usize> = None;

        // Collect events by date (index, title, kind, flag), applying filters
        #[allow(clippy::type_complexity)]
        let events_by_date: std::collections::HashMap<
            NaiveDate,
            Vec<(usize, String, u32, &'static str)>,
        > = {
            let mut dates: std::collections::HashMap<
                NaiveDate,
                Vec<(usize, String, u32, &'static str)>,
            > = std::collections::HashMap::new();
            for (idx, event) in self.events.iter().enumerate() {
                // Apply filters
                if !self.event_matches_filters(event, is_following) {
                    continue;
                }
                let flag = get_event_flag(event);
                dates.entry(event.start_date).or_default().push((
                    idx,
                    event.title.clone(),
                    event.kind,
                    flag,
                ));
            }
            dates
        };

        // Render calendar grid
        egui::Grid::new("month_grid")
            .num_columns(7)
            .min_col_width(cell_width)
            .max_col_width(cell_width)
            .min_row_height(cell_height)
            .spacing([2.0, 2.0])
            .show(ui, |ui| {
                for week in 0..6 {
                    for day in 0..7 {
                        let current_date = grid_start + chrono::Days::new((week * 7 + day) as u64);
                        let is_current_month = current_date.month() == month;
                        let is_today = current_date == today;

                        // Get events for this date
                        let day_events = events_by_date.get(&current_date);

                        // Make the entire cell clickable
                        let cell_response = ui.allocate_response(
                            Vec2::new(cell_width - 4.0, cell_height - 4.0),
                            egui::Sense::click(),
                        );

                        // Highlight cell on hover
                        if cell_response.hovered() {
                            ui.painter().rect_filled(
                                cell_response.rect,
                                CornerRadius::same(4),
                                Color32::from_rgba_unmultiplied(128, 128, 128, 30),
                            );
                        }

                        // Navigate to day on cell click
                        if cell_response.clicked() {
                            new_selected = Some(current_date);
                        }

                        // Draw content inside the cell
                        let content_rect = cell_response.rect.shrink(4.0);
                        let mut content_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(content_rect)
                                .layout(egui::Layout::top_down(egui::Align::LEFT)),
                        );

                        // Day number with today highlight
                        let text = current_date.day().to_string();
                        let mut rich_text = RichText::new(&text).size(16.0);

                        if !is_current_month {
                            rich_text = rich_text.weak();
                        }
                        if is_today {
                            rich_text = rich_text.strong().color(Color32::from_rgb(33, 150, 243));
                        }

                        content_ui.label(rich_text);

                        // Show events with colored indicators (up to 3)
                        if let Some(events) = day_events {
                            for (idx, title, kind, flag) in events.iter().take(3) {
                                let color = if *kind == KIND_DATE_CALENDAR_EVENT {
                                    date_event_color
                                } else {
                                    time_event_color
                                };

                                content_ui.horizontal(|ui| {
                                    // Shape indicator (different shapes for colorblind accessibility)
                                    let (rect, _) = ui.allocate_exact_size(
                                        Vec2::new(6.0, 6.0),
                                        egui::Sense::hover(),
                                    );
                                    if *kind == KIND_DATE_CALENDAR_EVENT {
                                        // Square for all-day events
                                        ui.painter().rect_filled(rect, CornerRadius::ZERO, color);
                                    } else {
                                        // Circle for timed events
                                        ui.painter().circle_filled(rect.center(), 3.0, color);
                                    }

                                    // Country flag (if available)
                                    if !flag.is_empty() {
                                        ui.label(RichText::new(*flag).size(12.0));
                                    }

                                    // Truncate long titles based on cell width (use chars for UTF-8 safety)
                                    // Reduce max_chars slightly if flag is shown
                                    let flag_offset = if flag.is_empty() { 0 } else { 2 };
                                    let max_chars =
                                        ((cell_width - 20.0) / 7.0) as usize - flag_offset;
                                    let display_title = if title.chars().count() > max_chars {
                                        let truncated: String =
                                            title.chars().take(max_chars).collect();
                                        format!("{}...", truncated)
                                    } else {
                                        title.clone()
                                    };

                                    let event_response = ui.add(
                                        egui::Label::new(RichText::new(display_title).small())
                                            .sense(egui::Sense::click()),
                                    );
                                    if event_response.clicked() {
                                        event_to_select = Some(*idx);
                                    }
                                });
                            }

                            if events.len() > 3 {
                                content_ui.label(
                                    RichText::new(format!("+{} more", events.len() - 3))
                                        .small()
                                        .weak(),
                                );
                            }
                        }
                    }
                    ui.end_row();
                }
            });

        // Apply selection changes after rendering
        if let Some(date) = new_selected {
            // Drill down to day view when clicking a day in month view
            self.drill_down_to_day(date);
        }
        if let Some(idx) = event_to_select {
            self.select_event(idx);
        }
    }

    /// Render week view.
    fn render_week_view<F>(&mut self, ui: &mut Ui, is_following: F)
    where
        F: Fn(&[u8; 32]) -> IsFollowing + Copy,
    {
        let date_event_color = Color32::from_rgb(76, 175, 80);
        let time_event_color = Color32::from_rgb(33, 150, 243);
        let today = Local::now().date_naive();

        // Calculate the start of the week (Sunday)
        let weekday = self.view_date.weekday().num_days_from_sunday();
        let week_start = self.view_date - chrono::Days::new(weekday as u64);

        // Generate days of the week
        let week_days: Vec<NaiveDate> = (0..7).map(|i| week_start + chrono::Days::new(i)).collect();

        // Collect events by date, applying filters
        let events_by_date: std::collections::HashMap<NaiveDate, Vec<(usize, &CachedEvent)>> = {
            let mut dates: std::collections::HashMap<NaiveDate, Vec<(usize, &CachedEvent)>> =
                std::collections::HashMap::new();
            for (idx, event) in self.events.iter().enumerate() {
                // Apply filters
                if !self.event_matches_filters(event, is_following) {
                    continue;
                }
                if week_days.contains(&event.start_date) {
                    dates
                        .entry(event.start_date)
                        .or_default()
                        .push((idx, event));
                }
            }
            dates
        };

        // Calculate column width
        let available_width = ui.available_width();
        let col_width = (available_width / 7.0).max(100.0);

        // Track interactions
        let mut day_to_select: Option<NaiveDate> = None;
        let mut event_to_select: Option<usize> = None;

        // Week header with day names and dates
        egui::Grid::new("week_header")
            .num_columns(7)
            .min_col_width(col_width)
            .max_col_width(col_width)
            .spacing([2.0, 4.0])
            .show(ui, |ui| {
                for date in &week_days {
                    let is_today = *date == today;
                    let day_name = date.format("%a").to_string();
                    let day_num = date.day().to_string();

                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(day_name).small());
                        let mut num_text = RichText::new(&day_num).size(18.0);
                        if is_today {
                            num_text = num_text.strong().color(Color32::from_rgb(33, 150, 243));
                        }
                        let response =
                            ui.add(egui::Label::new(num_text).sense(egui::Sense::click()));
                        if response.clicked() {
                            day_to_select = Some(*date);
                        }
                    });
                }
                ui.end_row();
            });

        ui.separator();

        // Event columns for each day
        egui::ScrollArea::vertical()
            .id_salt("week_view_events")
            .show(ui, |ui| {
                egui::Grid::new("week_events")
                    .num_columns(7)
                    .min_col_width(col_width)
                    .max_col_width(col_width)
                    .min_row_height(400.0)
                    .spacing([2.0, 0.0])
                    .show(ui, |ui| {
                        for date in &week_days {
                            ui.vertical(|ui| {
                                ui.set_min_width(col_width - 4.0);

                                if let Some(day_events) = events_by_date.get(date) {
                                    for (idx, event) in day_events {
                                        let color = if event.kind == KIND_DATE_CALENDAR_EVENT {
                                            date_event_color
                                        } else {
                                            time_event_color
                                        };

                                        let response = egui::Frame::new()
                                            .fill(color.gamma_multiply(0.2))
                                            .inner_margin(4.0)
                                            .corner_radius(CornerRadius::same(4))
                                            .show(ui, |ui| {
                                                ui.set_min_width(col_width - 12.0);

                                                // Time
                                                if let Some(ref start_time) = event.start_time {
                                                    let time_str = format_time(
                                                        start_time.timestamp,
                                                        start_time.timezone.as_deref(),
                                                    );
                                                    ui.label(RichText::new(time_str).small());
                                                }

                                                // Country flag + Title (truncated, UTF-8 safe)
                                                let flag = get_event_flag(event);
                                                let flag_offset =
                                                    if flag.is_empty() { 0 } else { 2 };
                                                let max_chars = ((col_width - 20.0) / 7.0) as usize
                                                    - flag_offset;
                                                let display_title =
                                                    if event.title.chars().count() > max_chars {
                                                        let truncated: String = event
                                                            .title
                                                            .chars()
                                                            .take(max_chars)
                                                            .collect();
                                                        format!("{}...", truncated)
                                                    } else {
                                                        event.title.clone()
                                                    };
                                                // Show flag + title
                                                let title_with_flag = if flag.is_empty() {
                                                    display_title
                                                } else {
                                                    format!("{} {}", flag, display_title)
                                                };
                                                ui.label(
                                                    RichText::new(title_with_flag).small().strong(),
                                                );
                                            });

                                        if response
                                            .response
                                            .interact(egui::Sense::click())
                                            .clicked()
                                        {
                                            event_to_select = Some(*idx);
                                        }

                                        ui.add_space(2.0);
                                    }
                                } else {
                                    ui.label(RichText::new("No events").weak().small());
                                }
                            });
                        }
                        ui.end_row();
                    });
            });

        // Apply interactions after rendering
        if let Some(date) = day_to_select {
            self.drill_down_to_day(date);
        }
        if let Some(idx) = event_to_select {
            self.select_event(idx);
        }
    }

    /// Render day view.
    fn render_day_view<F>(&mut self, ui: &mut Ui, is_following: F)
    where
        F: Fn(&[u8; 32]) -> IsFollowing + Copy,
    {
        ui.heading(self.selected_date.format("%A, %B %d, %Y").to_string());
        ui.separator();

        let date_event_color = Color32::from_rgb(76, 175, 80);
        let time_event_color = Color32::from_rgb(33, 150, 243);

        // Collect event indices for this date, applying filters
        let day_event_indices: Vec<usize> = self
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.start_date == self.selected_date && self.event_matches_filters(e, is_following)
            })
            .map(|(idx, _)| idx)
            .collect();

        let mut event_to_select: Option<usize> = None;

        if day_event_indices.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("No events scheduled for this day").weak());
        } else {
            egui::ScrollArea::vertical()
                .id_salt("day_view_events")
                .show(ui, |ui| {
                    for idx in day_event_indices {
                        let event = &self.events[idx];
                        let color = if event.kind == KIND_DATE_CALENDAR_EVENT {
                            date_event_color
                        } else {
                            time_event_color
                        };

                        let response = ui.group(|ui| {
                            ui.horizontal(|ui| {
                                // Color indicator bar
                                let (rect, _) = ui.allocate_exact_size(
                                    Vec2::new(4.0, 40.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(rect, CornerRadius::same(2), color);

                                ui.vertical(|ui| {
                                    // Title with country flag
                                    let flag = get_event_flag(event);
                                    let title_display = if flag.is_empty() {
                                        event.title.clone()
                                    } else {
                                        format!("{} {}", flag, event.title)
                                    };
                                    ui.label(RichText::new(&title_display).strong());

                                    // Time display
                                    if let Some(ref start_time) = event.start_time {
                                        let time_str = format_time(
                                            start_time.timestamp,
                                            start_time.timezone.as_deref(),
                                        );
                                        if let Some(ref end_time) = event.end_time {
                                            let end_str = format_time(
                                                end_time.timestamp,
                                                end_time.timezone.as_deref(),
                                            );
                                            ui.label(RichText::new(format!(
                                                "{} - {}",
                                                time_str, end_str
                                            )));
                                        } else {
                                            ui.label(RichText::new(time_str));
                                        }
                                    } else {
                                        ui.label(RichText::new("All day"));
                                    }

                                    // Location if present
                                    if !event.locations.is_empty() {
                                        ui.label(
                                            RichText::new(event.locations.join(", "))
                                                .small()
                                                .weak(),
                                        );
                                    }
                                });
                            });
                        });

                        if response.response.interact(egui::Sense::click()).clicked() {
                            event_to_select = Some(idx);
                        }
                    }
                });
        }

        if let Some(idx) = event_to_select {
            self.select_event(idx);
        }
    }

    /// Render agenda view.
    fn render_agenda_view<F>(&mut self, ui: &mut Ui, is_following: F)
    where
        F: Fn(&[u8; 32]) -> IsFollowing + Copy,
    {
        ui.heading("Upcoming Events");
        ui.separator();

        let today = Local::now().date_naive();
        let date_event_color = Color32::from_rgb(76, 175, 80);
        let time_event_color = Color32::from_rgb(33, 150, 243);

        // Get event indices from today onwards, sorted by date, applying filters
        let mut upcoming: Vec<(usize, NaiveDate)> = self
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.start_date >= today && self.event_matches_filters(e, is_following))
            .map(|(idx, e)| (idx, e.start_date))
            .collect();
        upcoming.sort_by_key(|(_, date)| *date);

        let mut event_to_select: Option<usize> = None;

        if upcoming.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("No upcoming events").weak());
        } else {
            egui::ScrollArea::vertical()
                .id_salt("agenda_view_events")
                .show(ui, |ui| {
                    let mut current_date: Option<NaiveDate> = None;

                    for (idx, date) in upcoming.iter().take(30) {
                        let event = &self.events[*idx];

                        // Date header
                        if current_date != Some(*date) {
                            current_date = Some(*date);
                            ui.add_space(12.0);

                            let is_today = *date == today;
                            let date_label = if is_today {
                                format!("Today - {}", date.format("%A, %B %d"))
                            } else {
                                date.format("%A, %B %d").to_string()
                            };

                            ui.label(RichText::new(date_label).strong().size(14.0));
                            ui.add_space(4.0);
                        }

                        let color = if event.kind == KIND_DATE_CALENDAR_EVENT {
                            date_event_color
                        } else {
                            time_event_color
                        };

                        // Event card
                        let response = ui.group(|ui| {
                            ui.horizontal(|ui| {
                                // Color indicator
                                let (rect, _) = ui.allocate_exact_size(
                                    Vec2::new(4.0, 36.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(rect, CornerRadius::same(2), color);

                                ui.vertical(|ui| {
                                    // Time + Flag + Title row
                                    ui.horizontal(|ui| {
                                        if let Some(ref start_time) = event.start_time {
                                            let time_str = format_time(
                                                start_time.timestamp,
                                                start_time.timezone.as_deref(),
                                            );
                                            ui.label(
                                                RichText::new(time_str).monospace().size(12.0),
                                            );
                                        } else {
                                            ui.label(RichText::new("All day").size(12.0));
                                        }

                                        // Country flag
                                        let flag = get_event_flag(event);
                                        if !flag.is_empty() {
                                            ui.label(RichText::new(flag));
                                        }

                                        ui.label(RichText::new(&event.title).strong());
                                    });

                                    // Location if present
                                    if !event.locations.is_empty() {
                                        ui.label(
                                            RichText::new(event.locations.join(", "))
                                                .small()
                                                .weak(),
                                        );
                                    }
                                });
                            });
                        });

                        if response.response.interact(egui::Sense::click()).clicked() {
                            event_to_select = Some(*idx);
                        }
                    }
                });
        }

        if let Some(idx) = event_to_select {
            self.select_event(idx);
        }
    }
}

impl App for CalendarApp {
    #[profiling::function]
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut Ui) -> AppResponse {
        // Process relay events (handles relay connections, messages, etc.)
        try_process_events_core(ctx, ui.ctx(), |_, _| {});

        // Ensure we have subscriptions and send to any newly connected relays
        self.ensure_subscription(ctx);
        self.ensure_comments_subscription(ctx);

        // Poll for new events and comments
        self.poll_events(ctx);
        self.poll_comments(ctx);

        // Refresh caches if needed
        self.refresh_events(ctx);
        self.refresh_comments(ctx);

        // Handle swipe gestures for back navigation (mobile)
        let is_mobile = ui.available_width() < 700.0;
        if is_mobile && self.can_go_back() {
            self.handle_swipe(ui);
        }

        // Extract context for passing to detail panel
        let ndb = &*ctx.ndb;
        let img_cache = &mut *ctx.img_cache;
        let media_jobs = &*ctx.media_jobs;

        // Create is_following closure for filtering
        let account = ctx.accounts.get_selected_account();
        let is_following = |pubkey: &[u8; 32]| account.is_following(pubkey);

        // Main layout with responsive detail panel
        ui.vertical(|ui| {
            // Add top padding for macOS window controls (traffic light buttons)
            #[cfg(target_os = "macos")]
            ui.add_space(28.0);

            self.render_header(ui);
            ui.separator();

            // Check if we should show the detail panel side-by-side or as overlay
            let available_width = ui.available_width();
            let show_side_panel = self.show_detail_panel && available_width > 700.0;

            if show_side_panel {
                // Desktop layout: calendar and detail side-by-side
                let available_height = ui.available_height();
                // Ensure detail panel has minimum 320pt width for readability
                let detail_width = (available_width * 0.40)
                    .max(320.0)
                    .min(available_width * 0.5);
                let calendar_width = available_width - detail_width - 8.0; // 8pt for separator

                ui.horizontal_top(|ui| {
                    // Calendar content (left side)
                    ui.vertical(|ui| {
                        ui.set_min_width(calendar_width);
                        ui.set_max_width(calendar_width);
                        ui.set_min_height(available_height);
                        self.render_content(ui, is_following);
                    });

                    ui.separator();

                    // Event detail panel (right side)
                    ui.vertical(|ui| {
                        ui.set_min_width(detail_width);
                        ui.set_max_width(detail_width);
                        ui.set_min_height(available_height);

                        egui::ScrollArea::vertical()
                            .id_salt("detail_panel_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_min_width(detail_width - 20.0);
                                self.render_event_detail(ui, ndb, img_cache, media_jobs);
                            });
                    });
                });
            } else if self.show_detail_panel {
                // Mobile layout: detail panel takes over the view
                // (Don't render calendar behind to avoid widget ID conflicts)
                ui.vertical(|ui| {
                    // Back button
                    if ui.button("← Back to Calendar").clicked() {
                        self.close_detail_panel();
                    }
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);

                    egui::ScrollArea::vertical()
                        .id_salt("mobile_detail_scroll")
                        .show(ui, |ui| {
                            self.render_event_detail(ui, ndb, img_cache, media_jobs);
                        });
                });
            } else {
                // Normal view without detail panel
                self.render_content(ui, is_following);
            }
        });

        AppResponse::default()
    }
}
