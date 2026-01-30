//! Main Git app struct and App trait implementation.

use crate::events::StatusKind;
use crate::subscriptions::GitSubscriptions;
use crate::ui::colors::{accent, bg, font, label, sizing, status, text};
use egui::{Color32, CornerRadius, RichText, Stroke, Vec2};
use nostrdb::Transaction;
use notedeck::{try_process_events_core, AppContext, AppResponse};

/// Navigation routes for the git app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRoute {
    /// Repository list view.
    RepoList,
    /// Repository detail view.
    RepoDetail { repo_id: String, owner: [u8; 32] },
    /// Patch list for a repository.
    PatchList { repo_id: String, owner: [u8; 32] },
    /// Patch detail view.
    PatchDetail { patch_key: nostrdb::NoteKey },
    /// Pull request list for a repository.
    PrList { repo_id: String, owner: [u8; 32] },
    /// Pull request detail view.
    PrDetail { pr_key: nostrdb::NoteKey },
    /// Issue list for a repository.
    IssueList { repo_id: String, owner: [u8; 32] },
    /// Issue detail view.
    IssueDetail { issue_key: nostrdb::NoteKey },
}

/// Actions that can be triggered by the git UI.
#[derive(Debug, Clone)]
pub enum GitAction {
    /// Navigate to a route.
    Navigate(GitRoute),
    /// Go back to previous route.
    Back,
}

/// Response from git UI rendering.
#[derive(Default)]
pub struct GitResponse {
    /// Optional action triggered by user interaction.
    pub action: Option<GitAction>,
}

impl GitResponse {
    /// Create a response with an action.
    pub fn action(action: GitAction) -> Self {
        Self {
            action: Some(action),
        }
    }
}

/// Filter for list views (Open vs Closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListFilter {
    #[default]
    Open,
    Closed,
}

/// The main Git app for Notedeck.
pub struct GitApp {
    /// Current navigation route.
    route: GitRoute,
    /// Navigation history for back button.
    history: Vec<GitRoute>,
    /// Git event subscriptions and cache.
    subs: GitSubscriptions,
    /// Current filter for issue list.
    issue_filter: ListFilter,
    /// Current filter for PR list.
    pr_filter: ListFilter,
    /// Current filter for patch list.
    patch_filter: ListFilter,
    /// Search query for repo list.
    repo_search: String,
    /// Search query for issue list.
    issue_search: String,
    /// Search query for PR list.
    pr_search: String,
    /// Search query for patch list.
    patch_search: String,
}

impl Default for GitApp {
    fn default() -> Self {
        Self::new()
    }
}

impl GitApp {
    /// Create a new Git app instance.
    pub fn new() -> Self {
        Self {
            route: GitRoute::RepoList,
            history: Vec::new(),
            subs: GitSubscriptions::new(),
            issue_filter: ListFilter::Open,
            pr_filter: ListFilter::Open,
            patch_filter: ListFilter::Open,
            repo_search: String::new(),
            issue_search: String::new(),
            pr_search: String::new(),
            patch_search: String::new(),
        }
    }

    /// Navigate to a new route, pushing current to history.
    fn navigate(&mut self, route: GitRoute) {
        self.history.push(self.route.clone());
        self.route = route;
    }

    /// Go back to previous route.
    fn go_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.route = prev;
        }
    }

    /// Get the repository address for a repo.
    fn repo_address(repo_id: &str, owner: &[u8; 32]) -> String {
        format!("30617:{}:{}", hex::encode(owner), repo_id)
    }

    /// Render a styled card container.
    fn render_card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
        egui::Frame::new()
            .fill(bg::CARD)
            .corner_radius(CornerRadius::same(sizing::CARD_ROUNDING))
            .stroke(Stroke::new(1.0, bg::BORDER))
            .inner_margin(egui::Margin::symmetric(
                sizing::CARD_PADDING_H,
                sizing::CARD_PADDING_V,
            ))
            .show(ui, content);
    }

    /// Render a search input field with GitHub-style appearance.
    fn render_search_field(ui: &mut egui::Ui, search_query: &mut String, placeholder: &str) {
        egui::Frame::new()
            .fill(bg::CARD)
            .corner_radius(CornerRadius::same(sizing::BADGE_ROUNDING))
            .stroke(Stroke::new(1.0, bg::BORDER))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Search icon (magnifying glass as text)
                    ui.label(RichText::new("🔍").size(font::BODY).color(text::TERTIARY));

                    ui.add_space(8.0);

                    // Text input
                    let response = ui.add(
                        egui::TextEdit::singleline(search_query)
                            .hint_text(placeholder)
                            .desired_width(ui.available_width() - 30.0)
                            .frame(false)
                            .text_color(text::PRIMARY)
                            .font(egui::TextStyle::Body),
                    );

                    // Clear button when there's text
                    if !search_query.is_empty()
                        && ui
                            .add(
                                egui::Button::new(
                                    RichText::new("✕").size(font::SMALL).color(text::TERTIARY),
                                )
                                .frame(false),
                            )
                            .clicked()
                    {
                        search_query.clear();
                        response.request_focus();
                    }
                });
            });
    }

    /// Render an interactive card that responds to clicks.
    fn render_interactive_card(
        ui: &mut egui::Ui,
        content: impl FnOnce(&mut egui::Ui),
    ) -> egui::Response {
        let frame = egui::Frame::new()
            .fill(bg::CARD)
            .corner_radius(CornerRadius::same(sizing::CARD_ROUNDING))
            .stroke(Stroke::new(1.0, bg::BORDER))
            .inner_margin(egui::Margin::symmetric(
                sizing::CARD_PADDING_H,
                sizing::CARD_PADDING_V,
            ));

        let frame_response = frame.show(ui, |ui| {
            // Ensure minimum touch target height
            ui.set_min_height(sizing::MIN_TOUCH_TARGET);
            content(ui);
        });

        // Make the entire card clickable and show hover state
        let response = frame_response.response.interact(egui::Sense::click());

        // Draw hover overlay if hovered
        if response.hovered() {
            ui.painter().rect_filled(
                frame_response.response.rect,
                CornerRadius::same(sizing::CARD_ROUNDING),
                Color32::from_rgba_unmultiplied(255, 255, 255, 8),
            );
        }

        response
    }

    /// Render a small badge/label with custom colors.
    fn render_badge(ui: &mut egui::Ui, text_content: &str, text_color: Color32, bg_color: Color32) {
        egui::Frame::new()
            .fill(bg_color)
            .corner_radius(CornerRadius::same(sizing::BADGE_ROUNDING))
            .inner_margin(egui::Margin::symmetric(6, 2))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(text_content)
                        .size(font::TINY)
                        .color(text_color),
                );
            });
    }

    /// Render a status badge (Open, Merged, Closed, Draft).
    #[allow(dead_code)]
    fn render_status_badge(ui: &mut egui::Ui, status_kind: StatusKind) {
        let (bg_color, text_str) = match status_kind {
            StatusKind::Open => (status::OPEN, "Open"),
            StatusKind::Applied => (status::MERGED, "Merged"),
            StatusKind::Closed => (status::CLOSED, "Closed"),
            StatusKind::Draft => (status::DRAFT, "Draft"),
        };
        Self::render_badge(ui, text_str, text::PRIMARY, bg_color);
    }

    /// Render an empty state message.
    fn render_empty_state(ui: &mut egui::Ui, message: &str) {
        Self::render_card(ui, |ui| {
            ui.label(RichText::new(message).color(text::SECONDARY));
        });
    }

    /// Render a styled back button.
    fn render_back_button(ui: &mut egui::Ui) -> bool {
        let button = egui::Button::new(
            RichText::new("← Back")
                .size(font::BODY)
                .color(text::SECONDARY),
        )
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .min_size(Vec2::new(
            sizing::MIN_TOUCH_TARGET,
            sizing::MIN_TOUCH_TARGET,
        ));

        ui.add(button).clicked()
    }

    /// Render a navigation tab button.
    fn render_tab_button(ui: &mut egui::Ui, label_text: &str, count: usize) -> bool {
        let button = egui::Button::new(
            RichText::new(format!("{} ({})", label_text, count))
                .size(font::BODY)
                .color(text::PRIMARY),
        )
        .fill(bg::HOVER)
        .stroke(Stroke::new(1.0, bg::BORDER))
        .corner_radius(CornerRadius::same(sizing::BADGE_ROUNDING))
        .min_size(Vec2::new(
            sizing::MIN_TOUCH_TARGET,
            sizing::MIN_TOUCH_TARGET,
        ));

        ui.add(button).clicked()
    }

    /// Render GitHub-style Open/Closed filter tabs.
    /// Returns the new filter if changed, None otherwise.
    fn render_filter_tabs(
        ui: &mut egui::Ui,
        current: ListFilter,
        open_count: usize,
        closed_count: usize,
    ) -> Option<ListFilter> {
        let mut new_filter = None;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            // Open tab
            let open_selected = current == ListFilter::Open;
            let open_text = format!("Open  {}", open_count);
            let open_button = egui::Button::new(
                RichText::new(&open_text)
                    .size(font::BODY)
                    .color(if open_selected {
                        text::PRIMARY
                    } else {
                        text::SECONDARY
                    })
                    .strong(),
            )
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .min_size(Vec2::new(80.0, sizing::MIN_TOUCH_TARGET));

            if ui.add(open_button).clicked() && !open_selected {
                new_filter = Some(ListFilter::Open);
            }

            // Show underline for selected
            if open_selected {
                let rect = ui.min_rect();
                ui.painter().hline(
                    rect.left()..=rect.left() + 80.0,
                    rect.bottom() - 2.0,
                    Stroke::new(2.0, status::OPEN),
                );
            }

            ui.add_space(sizing::SPACING_MD);

            // Closed tab
            let closed_selected = current == ListFilter::Closed;
            let closed_text = format!("Closed  {}", closed_count);
            let closed_button =
                egui::Button::new(RichText::new(&closed_text).size(font::BODY).color(
                    if closed_selected {
                        text::PRIMARY
                    } else {
                        text::SECONDARY
                    },
                ))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .min_size(Vec2::new(80.0, sizing::MIN_TOUCH_TARGET));

            if ui.add(closed_button).clicked() && !closed_selected {
                new_filter = Some(ListFilter::Closed);
            }
        });

        // Separator line
        ui.add_space(2.0);
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(
            rect.left()..=rect.right(),
            rect.top(),
            Stroke::new(1.0, bg::BORDER),
        );
        ui.add_space(sizing::SPACING_SM);

        new_filter
    }

    /// Render a status icon (colored circle).
    fn render_status_icon(ui: &mut egui::Ui, status_kind: StatusKind) {
        let color = match status_kind {
            StatusKind::Open => status::OPEN,
            StatusKind::Applied => status::MERGED,
            StatusKind::Closed => status::CLOSED,
            StatusKind::Draft => status::DRAFT,
        };
        let (_, rect) = ui.allocate_space(Vec2::splat(16.0));
        ui.painter().circle_filled(rect.center(), 5.0, color);
    }

    /// Format a relative time string (e.g., "2 days ago").
    fn format_relative_time(timestamp: u64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let diff = now.saturating_sub(timestamp);

        if diff < 60 {
            "just now".to_string()
        } else if diff < 3600 {
            let mins = diff / 60;
            format!("{} min{} ago", mins, if mins == 1 { "" } else { "s" })
        } else if diff < 86400 {
            let hours = diff / 3600;
            format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
        } else if diff < 604800 {
            let days = diff / 86400;
            format!("{} day{} ago", days, if days == 1 { "" } else { "s" })
        } else if diff < 2592000 {
            let weeks = diff / 604800;
            format!("{} week{} ago", weeks, if weeks == 1 { "" } else { "s" })
        } else {
            let months = diff / 2592000;
            format!("{} month{} ago", months, if months == 1 { "" } else { "s" })
        }
    }

    /// Truncate a URL to fit in limited space.
    fn truncate_url(url: &str, max_chars: usize) -> String {
        if url.len() <= max_chars {
            return url.to_string();
        }

        // Try to preserve the domain and show truncation
        if let Some(domain_end) = url.find("://").map(|i| {
            url[i + 3..]
                .find('/')
                .map(|j| i + 3 + j)
                .unwrap_or(url.len())
        }) {
            let domain = &url[..domain_end];
            if domain.len() < max_chars - 3 {
                // Show domain + start of path + ellipsis
                let remaining = max_chars - domain.len() - 3;
                if remaining > 5 {
                    return format!("{}{}...", domain, &url[domain_end..domain_end + remaining]);
                }
            }
        }

        // Fallback: just truncate with ellipsis
        format!("{}...", &url[..max_chars - 3])
    }

    /// Render the current view.
    #[profiling::function]
    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> GitResponse {
        // macOS needs space for window controls (traffic lights)
        if cfg!(target_os = "macos") {
            ui.add_space(24.0);
        }

        // Initialize subscriptions if needed (with wakeup for adding git relays)
        let egui_ctx = ui.ctx().clone();
        let wakeup = move || egui_ctx.request_repaint();
        self.subs.subscribe(ctx.ndb, ctx.pool, wakeup);

        // Poll for new events
        self.subs.poll(ctx.ndb);

        let mut response = GitResponse::default();

        // Header with back button
        if !matches!(self.route, GitRoute::RepoList) {
            ui.horizontal(|ui| {
                if Self::render_back_button(ui) {
                    response.action = Some(GitAction::Back);
                }
            });
            ui.add_space(sizing::SPACING_SM);
        }

        // If back was clicked, return early - don't render the rest
        if response.action.is_some() {
            return response;
        }

        // Render current route
        match self.route.clone() {
            GitRoute::RepoList => {
                response = self.render_repo_list(ctx, ui);
            }
            GitRoute::RepoDetail { repo_id, owner } => {
                response = self.render_repo_detail(ui, &repo_id, &owner);
            }
            GitRoute::IssueList { repo_id, owner } => {
                response = self.render_issue_list(ui, &repo_id, &owner);
            }
            GitRoute::PatchList { repo_id, owner } => {
                response = self.render_patch_list(ui, &repo_id, &owner);
            }
            GitRoute::PrList { repo_id, owner } => {
                response = self.render_pr_list(ui, &repo_id, &owner);
            }
            GitRoute::IssueDetail { issue_key } => {
                response = self.render_issue_detail(ctx, ui, issue_key);
            }
            GitRoute::PatchDetail { patch_key } => {
                response = self.render_patch_detail(ctx, ui, patch_key);
            }
            GitRoute::PrDetail { pr_key } => {
                response = self.render_pr_detail(ctx, ui, pr_key);
            }
        }

        response
    }

    /// Render repository list view.
    fn render_repo_list(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> GitResponse {
        let mut response = GitResponse::default();

        // Page heading
        ui.label(
            RichText::new("Git Repositories")
                .size(font::HEADING)
                .color(text::PRIMARY)
                .strong(),
        );
        ui.add_space(sizing::SPACING_SM);

        // Relay status indicator
        let relay_count = ctx.pool.relays.len();
        ui.horizontal(|ui| {
            if relay_count == 0 {
                ui.spinner();
                ui.label(RichText::new("Connecting to relays...").color(text::SECONDARY));
            } else {
                // Small dot indicator
                let dot_rect = ui.allocate_space(Vec2::splat(8.0)).1;
                ui.painter()
                    .circle_filled(dot_rect.center(), 4.0, accent::SUCCESS);
                ui.label(
                    RichText::new(format!("{} relay(s)", relay_count))
                        .size(font::SMALL)
                        .color(text::TERTIARY),
                );
            }
        });
        ui.add_space(sizing::SPACING_SM);

        // Search field
        Self::render_search_field(ui, &mut self.repo_search, "Search repositories...");
        ui.add_space(sizing::SPACING_MD);

        if self.subs.repos.is_empty() {
            // Loading state with card styling
            Self::render_card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Searching for NIP-34 git repositories...")
                            .color(text::SECONDARY),
                    );
                });
            });
            ui.add_space(sizing::SPACING_MD);

            // Connected relays collapsible
            egui::CollapsingHeader::new(
                RichText::new("Connected Relays")
                    .size(font::SMALL)
                    .color(text::TERTIARY),
            )
            .default_open(false)
            .show(ui, |ui| {
                for relay in &ctx.pool.relays {
                    ui.label(
                        RichText::new(relay.url())
                            .size(font::MONO)
                            .monospace()
                            .color(text::SECONDARY),
                    );
                }
            });

            ui.add_space(sizing::SPACING_SM);
            ui.label(
                RichText::new(
                    "Tip: NIP-34 git events are on specialized relays like relay.ngit.dev.",
                )
                .size(font::SMALL)
                .color(text::MUTED),
            );
            return response;
        }

        // Filter repos based on search query
        let search_lower = self.repo_search.to_lowercase();
        let filtered_repos: Vec<_> = self
            .subs
            .repos
            .iter()
            .filter(|repo| {
                if search_lower.is_empty() {
                    return true;
                }
                let name_match = repo.display_name().to_lowercase().contains(&search_lower);
                let desc_match = repo
                    .description
                    .as_ref()
                    .map(|d| d.to_lowercase().contains(&search_lower))
                    .unwrap_or(false);
                let label_match = repo
                    .labels
                    .iter()
                    .any(|l| l.to_lowercase().contains(&search_lower));
                name_match || desc_match || label_match
            })
            .collect();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for repo in filtered_repos {
                    // Get activity counts for this repo
                    let repo_address = Self::repo_address(&repo.id, &repo.owner);
                    let issue_count = self
                        .subs
                        .get_issues(&repo_address)
                        .map(|i| i.len())
                        .unwrap_or(0);
                    let patch_count = self
                        .subs
                        .get_patches(&repo_address)
                        .map(|p| p.len())
                        .unwrap_or(0);
                    let pr_count = self
                        .subs
                        .get_pull_requests(&repo_address)
                        .map(|p| p.len())
                        .unwrap_or(0);

                    let card_response = Self::render_interactive_card(ui, |ui| {
                        // Repository name row
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(repo.display_name())
                                    .size(font::TITLE)
                                    .color(text::PRIMARY)
                                    .strong(),
                            );
                            if repo.is_personal_fork {
                                Self::render_badge(ui, "fork", text::TERTIARY, bg::HOVER);
                            }
                        });

                        // Description
                        if let Some(desc) = &repo.description {
                            ui.add_space(sizing::SPACING_SM / 2.0);
                            ui.label(RichText::new(desc).size(font::BODY).color(text::SECONDARY));
                        }

                        // Labels row
                        if !repo.labels.is_empty() {
                            ui.add_space(sizing::SPACING_SM);
                            ui.horizontal_wrapped(|ui| {
                                for repo_label in &repo.labels {
                                    Self::render_badge(ui, repo_label, label::TEXT, label::BG);
                                }
                            });
                        }

                        // Activity stats row (always show for discoverability)
                        ui.add_space(sizing::SPACING_SM);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;

                            // Issues
                            let dot_rect = ui.allocate_space(Vec2::splat(8.0)).1;
                            ui.painter()
                                .circle_filled(dot_rect.center(), 3.0, status::OPEN);
                            ui.label(
                                RichText::new(format!("{}", issue_count))
                                    .size(font::SMALL)
                                    .color(if issue_count > 0 {
                                        text::SECONDARY
                                    } else {
                                        text::MUTED
                                    }),
                            );

                            // PRs
                            let dot_rect = ui.allocate_space(Vec2::splat(8.0)).1;
                            ui.painter()
                                .circle_filled(dot_rect.center(), 3.0, status::MERGED);
                            ui.label(
                                RichText::new(format!("{}", pr_count))
                                    .size(font::SMALL)
                                    .color(if pr_count > 0 {
                                        text::SECONDARY
                                    } else {
                                        text::MUTED
                                    }),
                            );

                            // Patches
                            let dot_rect = ui.allocate_space(Vec2::splat(8.0)).1;
                            ui.painter()
                                .circle_filled(dot_rect.center(), 3.0, accent::LINK);
                            ui.label(
                                RichText::new(format!("{}", patch_count))
                                    .size(font::SMALL)
                                    .color(if patch_count > 0 {
                                        text::SECONDARY
                                    } else {
                                        text::MUTED
                                    }),
                            );
                        });

                        // Clone URL (truncated)
                        if !repo.clone_urls.is_empty() {
                            ui.add_space(sizing::SPACING_SM);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Clone:")
                                        .size(font::SMALL)
                                        .color(text::TERTIARY),
                                );
                                let truncated = Self::truncate_url(&repo.clone_urls[0], 45);
                                ui.label(
                                    RichText::new(truncated)
                                        .size(font::MONO)
                                        .monospace()
                                        .color(accent::LINK),
                                );
                            });
                        }
                    });

                    if card_response.clicked() {
                        response.action = Some(GitAction::Navigate(GitRoute::RepoDetail {
                            repo_id: repo.id.clone(),
                            owner: repo.owner,
                        }));
                    }

                    ui.add_space(sizing::CARD_SPACING);
                }
            });

        response
    }

    /// Render repository detail view with tabs.
    fn render_repo_detail(
        &self,
        ui: &mut egui::Ui,
        repo_id: &str,
        owner: &[u8; 32],
    ) -> GitResponse {
        let mut response = GitResponse::default();

        // Find the repo
        let repo = self
            .subs
            .repos
            .iter()
            .find(|r| r.id == *repo_id && r.owner == *owner);

        let Some(repo) = repo else {
            Self::render_empty_state(ui, "Repository not found");
            return response;
        };

        // Repo header card
        Self::render_card(ui, |ui| {
            ui.label(
                RichText::new(repo.display_name())
                    .size(font::HEADING)
                    .color(text::PRIMARY)
                    .strong(),
            );

            if let Some(desc) = &repo.description {
                ui.add_space(sizing::SPACING_SM);
                ui.label(RichText::new(desc).size(font::BODY).color(text::SECONDARY));
            }

            // Clone URLs
            if !repo.clone_urls.is_empty() {
                ui.add_space(sizing::SPACING_MD);
                for url in &repo.clone_urls {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Clone:")
                                .size(font::SMALL)
                                .color(text::TERTIARY),
                        );
                        let truncated = Self::truncate_url(url, 50);
                        ui.label(
                            RichText::new(truncated)
                                .size(font::MONO)
                                .monospace()
                                .color(accent::LINK),
                        );
                    });
                }
            }
        });

        ui.add_space(sizing::SPACING_LG);

        // Get counts from subscriptions cache
        let repo_address = Self::repo_address(repo_id, owner);
        let issue_count = self
            .subs
            .get_issues(&repo_address)
            .map(|i| i.len())
            .unwrap_or(0);
        let patch_count = self
            .subs
            .get_patches(&repo_address)
            .map(|p| p.len())
            .unwrap_or(0);
        let pr_count = self
            .subs
            .get_pull_requests(&repo_address)
            .map(|p| p.len())
            .unwrap_or(0);

        // Navigation tabs
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = sizing::SPACING_SM;

            if Self::render_tab_button(ui, "Issues", issue_count) {
                response.action = Some(GitAction::Navigate(GitRoute::IssueList {
                    repo_id: repo_id.to_string(),
                    owner: *owner,
                }));
            }

            if Self::render_tab_button(ui, "Patches", patch_count) {
                response.action = Some(GitAction::Navigate(GitRoute::PatchList {
                    repo_id: repo_id.to_string(),
                    owner: *owner,
                }));
            }

            if Self::render_tab_button(ui, "Pull Requests", pr_count) {
                response.action = Some(GitAction::Navigate(GitRoute::PrList {
                    repo_id: repo_id.to_string(),
                    owner: *owner,
                }));
            }
        });

        response
    }

    /// Render issue list with GitHub-style Open/Closed tabs.
    fn render_issue_list(
        &mut self,
        ui: &mut egui::Ui,
        repo_id: &str,
        owner: &[u8; 32],
    ) -> GitResponse {
        let mut response = GitResponse::default();

        let repo_address = Self::repo_address(repo_id, owner);
        let issues = self.subs.get_issues(&repo_address);

        ui.label(
            RichText::new("Issues")
                .size(font::HEADING)
                .color(text::PRIMARY)
                .strong(),
        );
        ui.add_space(sizing::SPACING_SM);

        // Search field
        Self::render_search_field(ui, &mut self.issue_search, "Search issues...");
        ui.add_space(sizing::SPACING_MD);

        let Some(issues) = issues else {
            Self::render_empty_state(ui, "No issues found for this repository.");
            return response;
        };

        if issues.is_empty() {
            Self::render_empty_state(ui, "No issues found for this repository.");
            return response;
        }

        // Filter by search query
        let search_lower = self.issue_search.to_lowercase();
        let filtered_issues: Vec<_> = issues
            .iter()
            .filter(|issue| {
                if search_lower.is_empty() {
                    return true;
                }
                let title_match = issue.display_title().to_lowercase().contains(&search_lower);
                let content_match = issue.content.to_lowercase().contains(&search_lower);
                let label_match = issue
                    .labels
                    .iter()
                    .any(|l| l.to_lowercase().contains(&search_lower));
                title_match || content_match || label_match
            })
            .collect();

        // Count open vs closed (from filtered)
        let mut open_count = 0;
        let mut closed_count = 0;
        for issue in filtered_issues.iter() {
            let issue_id = hex::encode(issue.key.as_u64().to_be_bytes());
            let issue_status = self.subs.get_status(&issue_id).unwrap_or(StatusKind::Open);
            match issue_status {
                StatusKind::Open | StatusKind::Draft => open_count += 1,
                StatusKind::Closed | StatusKind::Applied => closed_count += 1,
            }
        }

        // Filter tabs
        if let Some(new_filter) =
            Self::render_filter_tabs(ui, self.issue_filter, open_count, closed_count)
        {
            self.issue_filter = new_filter;
        }

        // Filtered list
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for issue in filtered_issues.iter() {
                    let issue_id = hex::encode(issue.key.as_u64().to_be_bytes());
                    let current_status =
                        self.subs.get_status(&issue_id).unwrap_or(StatusKind::Open);

                    // Filter based on current tab
                    let show = match self.issue_filter {
                        ListFilter::Open => {
                            matches!(current_status, StatusKind::Open | StatusKind::Draft)
                        }
                        ListFilter::Closed => {
                            matches!(current_status, StatusKind::Closed | StatusKind::Applied)
                        }
                    };
                    if !show {
                        continue;
                    }

                    // GitHub-style row with wrapping support
                    let card_response = Self::render_interactive_card(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Status icon (fixed width)
                            Self::render_status_icon(ui, current_status);
                            ui.add_space(sizing::SPACING_SM);

                            // Content area (fills remaining width)
                            ui.vertical(|ui| {
                                // Title with wrapping
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(issue.display_title())
                                            .size(font::BODY)
                                            .color(text::PRIMARY)
                                            .strong(),
                                    )
                                    .wrap(),
                                );

                                // Labels on separate line
                                if !issue.labels.is_empty() {
                                    ui.horizontal_wrapped(|ui| {
                                        for issue_label in &issue.labels {
                                            Self::render_badge(
                                                ui,
                                                issue_label,
                                                label::TEXT,
                                                label::BG,
                                            );
                                        }
                                    });
                                }

                                // Metadata row
                                ui.label(
                                    RichText::new(format!(
                                        "opened {}",
                                        Self::format_relative_time(issue.created_at)
                                    ))
                                    .size(font::SMALL)
                                    .color(text::TERTIARY),
                                );
                            });
                        });
                    });

                    if card_response.clicked() {
                        response.action = Some(GitAction::Navigate(GitRoute::IssueDetail {
                            issue_key: issue.key,
                        }));
                    }

                    ui.add_space(sizing::CARD_SPACING);
                }
            });

        response
    }

    /// Render patch list with GitHub-style Open/Closed tabs.
    fn render_patch_list(
        &mut self,
        ui: &mut egui::Ui,
        repo_id: &str,
        owner: &[u8; 32],
    ) -> GitResponse {
        let mut response = GitResponse::default();

        let repo_address = Self::repo_address(repo_id, owner);
        let patches = self.subs.get_patches(&repo_address);

        ui.label(
            RichText::new("Patches")
                .size(font::HEADING)
                .color(text::PRIMARY)
                .strong(),
        );
        ui.add_space(sizing::SPACING_SM);

        // Search field
        Self::render_search_field(ui, &mut self.patch_search, "Search patches...");
        ui.add_space(sizing::SPACING_MD);

        let Some(patches) = patches else {
            Self::render_empty_state(ui, "No patches found for this repository.");
            return response;
        };

        if patches.is_empty() {
            Self::render_empty_state(ui, "No patches found for this repository.");
            return response;
        }

        // Filter by search query
        let search_lower = self.patch_search.to_lowercase();
        let filtered_patches: Vec<_> = patches
            .iter()
            .filter(|patch| {
                if search_lower.is_empty() {
                    return true;
                }
                let title_match = patch
                    .subject()
                    .map(|s| s.to_lowercase().contains(&search_lower))
                    .unwrap_or(false);
                let content_match = patch.content.to_lowercase().contains(&search_lower);
                title_match || content_match
            })
            .collect();

        // Count open vs closed/merged
        let mut open_count = 0;
        let mut closed_count = 0;
        for patch in filtered_patches.iter() {
            let patch_id = hex::encode(patch.key.as_u64().to_be_bytes());
            let patch_status = self.subs.get_status(&patch_id).unwrap_or(StatusKind::Open);
            match patch_status {
                StatusKind::Open | StatusKind::Draft => open_count += 1,
                StatusKind::Closed | StatusKind::Applied => closed_count += 1,
            }
        }

        // Filter tabs
        if let Some(new_filter) =
            Self::render_filter_tabs(ui, self.patch_filter, open_count, closed_count)
        {
            self.patch_filter = new_filter;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for patch in filtered_patches.iter() {
                    let patch_id = hex::encode(patch.key.as_u64().to_be_bytes());
                    let current_status =
                        self.subs.get_status(&patch_id).unwrap_or(StatusKind::Open);

                    // Filter based on current tab
                    let show = match self.patch_filter {
                        ListFilter::Open => {
                            matches!(current_status, StatusKind::Open | StatusKind::Draft)
                        }
                        ListFilter::Closed => {
                            matches!(current_status, StatusKind::Closed | StatusKind::Applied)
                        }
                    };
                    if !show {
                        continue;
                    }

                    let title = patch.subject().unwrap_or("Untitled Patch");

                    // GitHub-style row with wrapping support
                    let card_response = Self::render_interactive_card(ui, |ui| {
                        ui.horizontal(|ui| {
                            Self::render_status_icon(ui, current_status);
                            ui.add_space(sizing::SPACING_SM);

                            ui.vertical(|ui| {
                                // Title with wrapping
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(title)
                                            .size(font::BODY)
                                            .color(text::PRIMARY)
                                            .strong(),
                                    )
                                    .wrap(),
                                );

                                if patch.is_root {
                                    Self::render_badge(ui, "root", text::TERTIARY, bg::HOVER);
                                }

                                // Metadata row
                                ui.horizontal(|ui| {
                                    if let Some(commit) = &patch.commit_id {
                                        let short = if commit.len() > 7 {
                                            &commit[..7]
                                        } else {
                                            commit
                                        };
                                        ui.label(
                                            RichText::new(short)
                                                .size(font::SMALL)
                                                .monospace()
                                                .color(text::TERTIARY),
                                        );
                                        ui.label(
                                            RichText::new(" · ")
                                                .size(font::SMALL)
                                                .color(text::MUTED),
                                        );
                                    }
                                    ui.label(
                                        RichText::new(Self::format_relative_time(patch.created_at))
                                            .size(font::SMALL)
                                            .color(text::TERTIARY),
                                    );
                                });
                            });
                        });
                    });

                    if card_response.clicked() {
                        response.action = Some(GitAction::Navigate(GitRoute::PatchDetail {
                            patch_key: patch.key,
                        }));
                    }

                    ui.add_space(sizing::CARD_SPACING);
                }
            });

        response
    }

    /// Render PR list with GitHub-style Open/Closed tabs.
    fn render_pr_list(
        &mut self,
        ui: &mut egui::Ui,
        repo_id: &str,
        owner: &[u8; 32],
    ) -> GitResponse {
        let mut response = GitResponse::default();

        let repo_address = Self::repo_address(repo_id, owner);
        let prs = self.subs.get_pull_requests(&repo_address);

        ui.label(
            RichText::new("Pull Requests")
                .size(font::HEADING)
                .color(text::PRIMARY)
                .strong(),
        );
        ui.add_space(sizing::SPACING_SM);

        // Search field
        Self::render_search_field(ui, &mut self.pr_search, "Search pull requests...");
        ui.add_space(sizing::SPACING_MD);

        let Some(prs) = prs else {
            Self::render_empty_state(ui, "No pull requests found for this repository.");
            return response;
        };

        if prs.is_empty() {
            Self::render_empty_state(ui, "No pull requests found for this repository.");
            return response;
        }

        // Filter by search query
        let search_lower = self.pr_search.to_lowercase();
        let filtered_prs: Vec<_> = prs
            .iter()
            .filter(|pr| {
                if search_lower.is_empty() {
                    return true;
                }
                let title_match = pr.display_title().to_lowercase().contains(&search_lower);
                let content_match = pr.content.to_lowercase().contains(&search_lower);
                let label_match = pr
                    .labels
                    .iter()
                    .any(|l| l.to_lowercase().contains(&search_lower));
                title_match || content_match || label_match
            })
            .collect();

        // Count open vs closed/merged
        let mut open_count = 0;
        let mut closed_count = 0;
        for pr in filtered_prs.iter() {
            let pr_id = hex::encode(pr.key.as_u64().to_be_bytes());
            let pr_status = self.subs.get_status(&pr_id).unwrap_or(StatusKind::Open);
            match pr_status {
                StatusKind::Open | StatusKind::Draft => open_count += 1,
                StatusKind::Closed | StatusKind::Applied => closed_count += 1,
            }
        }

        // Filter tabs
        if let Some(new_filter) =
            Self::render_filter_tabs(ui, self.pr_filter, open_count, closed_count)
        {
            self.pr_filter = new_filter;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for pr in filtered_prs.iter() {
                    let pr_id = hex::encode(pr.key.as_u64().to_be_bytes());
                    let current_status = self.subs.get_status(&pr_id).unwrap_or(StatusKind::Open);

                    // Filter based on current tab
                    let show = match self.pr_filter {
                        ListFilter::Open => {
                            matches!(current_status, StatusKind::Open | StatusKind::Draft)
                        }
                        ListFilter::Closed => {
                            matches!(current_status, StatusKind::Closed | StatusKind::Applied)
                        }
                    };
                    if !show {
                        continue;
                    }

                    // GitHub-style row with wrapping support
                    let card_response = Self::render_interactive_card(ui, |ui| {
                        ui.horizontal(|ui| {
                            Self::render_status_icon(ui, current_status);
                            ui.add_space(sizing::SPACING_SM);

                            ui.vertical(|ui| {
                                // Title with wrapping
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(pr.display_title())
                                            .size(font::BODY)
                                            .color(text::PRIMARY)
                                            .strong(),
                                    )
                                    .wrap(),
                                );

                                // Labels on separate line
                                if !pr.labels.is_empty() {
                                    ui.horizontal_wrapped(|ui| {
                                        for pr_label in &pr.labels {
                                            Self::render_badge(
                                                ui,
                                                pr_label,
                                                label::TEXT,
                                                label::BG,
                                            );
                                        }
                                    });
                                }

                                // Metadata row
                                ui.horizontal(|ui| {
                                    if let Some(commit) = &pr.commit_id {
                                        let short = if commit.len() > 7 {
                                            &commit[..7]
                                        } else {
                                            commit
                                        };
                                        ui.label(
                                            RichText::new(short)
                                                .size(font::SMALL)
                                                .monospace()
                                                .color(text::TERTIARY),
                                        );
                                        ui.label(
                                            RichText::new(" · ")
                                                .size(font::SMALL)
                                                .color(text::MUTED),
                                        );
                                    }
                                    ui.label(
                                        RichText::new(format!(
                                            "opened {}",
                                            Self::format_relative_time(pr.created_at)
                                        ))
                                        .size(font::SMALL)
                                        .color(text::TERTIARY),
                                    );
                                });
                            });
                        });
                    });

                    if card_response.clicked() {
                        response.action =
                            Some(GitAction::Navigate(GitRoute::PrDetail { pr_key: pr.key }));
                    }

                    ui.add_space(sizing::CARD_SPACING);
                }
            });

        response
    }

    /// Render issue detail view with GitHub-style layout.
    fn render_issue_detail(
        &self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        issue_key: nostrdb::NoteKey,
    ) -> GitResponse {
        let Ok(txn) = Transaction::new(ctx.ndb) else {
            Self::render_empty_state(ui, "Failed to open transaction");
            return GitResponse::default();
        };

        let Ok(note) = ctx.ndb.get_note_by_key(&txn, issue_key) else {
            Self::render_empty_state(ui, "Issue not found");
            return GitResponse::default();
        };

        let Some(issue) = crate::events::GitIssue::from_note(&note) else {
            Self::render_empty_state(ui, "Invalid issue");
            return GitResponse::default();
        };

        // Get status
        let issue_id = hex::encode(issue.key.as_u64().to_be_bytes());
        let current_status = self.subs.get_status(&issue_id).unwrap_or(StatusKind::Open);

        // Title (wrapping enabled for long titles)
        ui.label(
            RichText::new(issue.display_title())
                .size(font::HEADING)
                .color(text::PRIMARY)
                .strong(),
        );

        ui.add_space(sizing::SPACING_SM);

        // Status badge
        ui.horizontal(|ui| {
            Self::render_status_badge(ui, current_status);
        });

        ui.add_space(sizing::SPACING_MD);

        // Author info row
        ui.horizontal(|ui| {
            // Author avatar placeholder (colored circle)
            let (_, rect) = ui.allocate_space(Vec2::splat(24.0));
            ui.painter()
                .circle_filled(rect.center(), 10.0, accent::PRIMARY);

            ui.add_space(sizing::SPACING_SM);

            ui.label(
                RichText::new(format!(
                    "opened {}",
                    Self::format_relative_time(issue.created_at)
                ))
                .size(font::SMALL)
                .color(text::SECONDARY),
            );
        });

        ui.add_space(sizing::SPACING_MD);

        // Labels row
        if !issue.labels.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for issue_label in &issue.labels {
                    Self::render_badge(ui, issue_label, label::TEXT, label::BG);
                }
            });
            ui.add_space(sizing::SPACING_MD);
        }

        // Content body
        Self::render_card(ui, |ui| {
            if issue.content.trim().is_empty() {
                ui.label(
                    RichText::new("No description provided.")
                        .size(font::BODY)
                        .color(text::MUTED)
                        .italics(),
                );
            } else {
                ui.label(
                    RichText::new(&issue.content)
                        .size(font::BODY)
                        .color(text::PRIMARY),
                );
            }
        });

        ui.add_space(sizing::SPACING_LG);

        // Metadata section
        Self::render_card(ui, |ui| {
            ui.label(
                RichText::new("Details")
                    .size(font::SMALL)
                    .color(text::TERTIARY)
                    .strong(),
            );
            ui.add_space(sizing::SPACING_SM);

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Status:")
                        .size(font::SMALL)
                        .color(text::TERTIARY),
                );
                ui.label(
                    RichText::new(format!("{:?}", current_status))
                        .size(font::SMALL)
                        .color(text::SECONDARY),
                );
            });

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Created:")
                        .size(font::SMALL)
                        .color(text::TERTIARY),
                );
                ui.label(
                    RichText::new(Self::format_relative_time(issue.created_at))
                        .size(font::SMALL)
                        .color(text::SECONDARY),
                );
            });
        });

        GitResponse::default()
    }

    /// Render patch detail view with GitHub-style layout.
    fn render_patch_detail(
        &self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        patch_key: nostrdb::NoteKey,
    ) -> GitResponse {
        let Ok(txn) = Transaction::new(ctx.ndb) else {
            Self::render_empty_state(ui, "Failed to open transaction");
            return GitResponse::default();
        };

        let Ok(note) = ctx.ndb.get_note_by_key(&txn, patch_key) else {
            Self::render_empty_state(ui, "Patch not found");
            return GitResponse::default();
        };

        let Some(patch) = crate::events::GitPatch::from_note(&note) else {
            Self::render_empty_state(ui, "Invalid patch");
            return GitResponse::default();
        };

        // Get status
        let patch_id = hex::encode(patch.key.as_u64().to_be_bytes());
        let current_status = self.subs.get_status(&patch_id).unwrap_or(StatusKind::Open);

        let title = patch.subject().unwrap_or("Untitled Patch");

        // Title
        ui.label(
            RichText::new(title)
                .size(font::HEADING)
                .color(text::PRIMARY)
                .strong(),
        );

        ui.add_space(sizing::SPACING_SM);

        // Status badge and metadata
        ui.horizontal(|ui| {
            Self::render_status_badge(ui, current_status);
            ui.add_space(sizing::SPACING_MD);

            if let Some(commit) = &patch.commit_id {
                let short = if commit.len() > 7 {
                    &commit[..7]
                } else {
                    commit
                };
                ui.label(
                    RichText::new(short)
                        .size(font::MONO)
                        .monospace()
                        .color(accent::LINK),
                );
                ui.label(RichText::new(" · ").size(font::SMALL).color(text::MUTED));
            }

            ui.label(
                RichText::new(Self::format_relative_time(patch.created_at))
                    .size(font::SMALL)
                    .color(text::TERTIARY),
            );
        });

        ui.add_space(sizing::SPACING_MD);

        // Patch content with monospace in a code-style card
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x0D, 0x11, 0x17)) // Darker code background
                    .corner_radius(CornerRadius::same(sizing::CARD_ROUNDING))
                    .stroke(Stroke::new(1.0, bg::BORDER))
                    .inner_margin(egui::Margin::symmetric(
                        sizing::CARD_PADDING_H,
                        sizing::CARD_PADDING_V,
                    ))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&patch.content)
                                .size(font::MONO)
                                .monospace()
                                .color(text::PRIMARY),
                        );
                    });
            });

        GitResponse::default()
    }

    /// Render PR detail view with GitHub-style layout.
    fn render_pr_detail(
        &self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        pr_key: nostrdb::NoteKey,
    ) -> GitResponse {
        let Ok(txn) = Transaction::new(ctx.ndb) else {
            Self::render_empty_state(ui, "Failed to open transaction");
            return GitResponse::default();
        };

        let Ok(note) = ctx.ndb.get_note_by_key(&txn, pr_key) else {
            Self::render_empty_state(ui, "Pull request not found");
            return GitResponse::default();
        };

        let Some(pr) = crate::events::GitPullRequest::from_note(&note) else {
            Self::render_empty_state(ui, "Invalid pull request");
            return GitResponse::default();
        };

        // Get status
        let pr_id = hex::encode(pr.key.as_u64().to_be_bytes());
        let current_status = self.subs.get_status(&pr_id).unwrap_or(StatusKind::Open);

        // Title
        ui.label(
            RichText::new(pr.display_title())
                .size(font::HEADING)
                .color(text::PRIMARY)
                .strong(),
        );

        ui.add_space(sizing::SPACING_SM);

        // Status badge
        ui.horizontal(|ui| {
            Self::render_status_badge(ui, current_status);
        });

        ui.add_space(sizing::SPACING_MD);

        // Author info row
        ui.horizontal(|ui| {
            let (_, rect) = ui.allocate_space(Vec2::splat(24.0));
            ui.painter()
                .circle_filled(rect.center(), 10.0, status::MERGED);

            ui.add_space(sizing::SPACING_SM);

            ui.label(
                RichText::new(format!(
                    "opened {}",
                    Self::format_relative_time(pr.created_at)
                ))
                .size(font::SMALL)
                .color(text::SECONDARY),
            );
        });

        ui.add_space(sizing::SPACING_MD);

        // Labels row
        if !pr.labels.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for pr_label in &pr.labels {
                    Self::render_badge(ui, pr_label, label::TEXT, label::BG);
                }
            });
            ui.add_space(sizing::SPACING_MD);
        }

        // Content body
        Self::render_card(ui, |ui| {
            if pr.content.trim().is_empty() {
                ui.label(
                    RichText::new("No description provided.")
                        .size(font::BODY)
                        .color(text::MUTED)
                        .italics(),
                );
            } else {
                ui.label(
                    RichText::new(&pr.content)
                        .size(font::BODY)
                        .color(text::PRIMARY),
                );
            }
        });

        ui.add_space(sizing::SPACING_LG);

        // Details card
        Self::render_card(ui, |ui| {
            ui.label(
                RichText::new("Details")
                    .size(font::SMALL)
                    .color(text::TERTIARY)
                    .strong(),
            );
            ui.add_space(sizing::SPACING_SM);

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Status:")
                        .size(font::SMALL)
                        .color(text::TERTIARY),
                );
                ui.label(
                    RichText::new(format!("{:?}", current_status))
                        .size(font::SMALL)
                        .color(text::SECONDARY),
                );
            });

            // Commit
            if let Some(commit) = &pr.commit_id {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Commit:")
                            .size(font::SMALL)
                            .color(text::TERTIARY),
                    );
                    let short = if commit.len() > 7 {
                        &commit[..7]
                    } else {
                        commit
                    };
                    ui.label(
                        RichText::new(short)
                            .size(font::MONO)
                            .monospace()
                            .color(accent::LINK),
                    );
                });
            }

            // Clone URLs
            if !pr.clone_urls.is_empty() {
                ui.add_space(sizing::SPACING_SM);
                ui.label(
                    RichText::new("Clone URLs:")
                        .size(font::SMALL)
                        .color(text::TERTIARY),
                );
                for url in &pr.clone_urls {
                    let truncated = Self::truncate_url(url, 50);
                    ui.label(
                        RichText::new(truncated)
                            .size(font::TINY)
                            .monospace()
                            .color(accent::LINK),
                    );
                }
            }
        });

        GitResponse::default()
    }
}

impl notedeck::App for GitApp {
    #[profiling::function]
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        // Process relay events to ingest into nostrdb
        try_process_events_core(ctx, ui.ctx(), |_, _| {});

        // Main container with dark background and consistent padding
        egui::Frame::new()
            .fill(bg::PRIMARY)
            .inner_margin(egui::Margin::symmetric(16, 16))
            .show(ui, |ui| {
                let response = self.render(ctx, ui);

                // Handle actions
                if let Some(action) = response.action {
                    match action {
                        GitAction::Navigate(route) => self.navigate(route),
                        GitAction::Back => self.go_back(),
                    }
                }
            });

        AppResponse::none()
    }
}
