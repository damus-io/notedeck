use egui::Ui;
use egui_nav::{Nav, NavAction};
use nostrdb::Ndb;

use crate::{
    account_manager::{process_login_view_response, AccountManager},
    imgcache::ImageCache,
    login_manager::LoginState,
    routable_widget_state::RoutableWidgetState,
    route::{ManageAccountRoute, ManageAcountRouteResponse},
    Damus,
};

use super::{
    account_login_view::AccountLoginView, account_management::AccountManagementViewResponse,
    AccountManagementView,
};

pub struct StatefulAccountManagementView {}

impl StatefulAccountManagementView {
    pub fn show(
        ui: &mut Ui,
        account_management_state: &mut RoutableWidgetState<ManageAccountRoute>,
        account_manager: &mut AccountManager,
        img_cache: &mut ImageCache,
        login_state: &mut LoginState,
        ndb: &Ndb,
    ) {
        let routes = account_management_state.get_routes();

        let nav_response =
            Nav::new(routes)
                .title(false)
                .navigating(false)
                .show_mut(ui, |ui, nav| match nav.top() {
                    ManageAccountRoute::AccountManagement => {
                        AccountManagementView::ui(ui, account_manager, ndb, img_cache)
                            .inner
                            .map(ManageAcountRouteResponse::AccountManagement)
                    }
                    ManageAccountRoute::AddAccount => AccountLoginView::new(login_state)
                        .ui(ui)
                        .inner
                        .map(ManageAcountRouteResponse::AddAccount),
                });

        if let Some(resp) = nav_response.inner {
            match resp {
                ManageAcountRouteResponse::AccountManagement(response) => {
                    process_management_view_response_stateful(
                        response,
                        account_manager,
                        account_management_state,
                    );
                }
                ManageAcountRouteResponse::AddAccount(response) => {
                    process_login_view_response(account_manager, response);
                    *login_state = Default::default();
                    account_management_state.go_back();
                }
            }
        }
        if let Some(NavAction::Returned) = nav_response.action {
            account_management_state.go_back();
        }
    }
}

pub fn process_management_view_response_stateful(
    response: AccountManagementViewResponse,
    manager: &mut AccountManager,
    state: &mut RoutableWidgetState<ManageAccountRoute>,
) {
    match response {
        AccountManagementViewResponse::RemoveAccount(index) => {
            manager.remove_account(index);
        }
        AccountManagementViewResponse::SelectAccount(index) => {
            manager.select_account(index);
        }
        AccountManagementViewResponse::RouteToLogin => {
            state.route_to(ManageAccountRoute::AddAccount);
        }
    }
}

mod preview {
    use crate::{
        test_data,
        ui::{Preview, PreviewConfig, View},
    };

    use super::*;

    pub struct StatefulAccountManagementPreview {
        app: Damus,
    }

    impl StatefulAccountManagementPreview {
        fn new() -> Self {
            let mut app = test_data::test_app();
            app.account_management_view_state
                .route_to(ManageAccountRoute::AccountManagement);

            StatefulAccountManagementPreview { app }
        }
    }

    impl View for StatefulAccountManagementPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            StatefulAccountManagementView::show(
                ui,
                &mut self.app.account_management_view_state,
                &mut self.app.accounts,
                &mut self.app.img_cache,
                &mut self.app.login_state,
                &self.app.ndb,
            );
        }
    }

    impl Preview for StatefulAccountManagementView {
        type Prev = StatefulAccountManagementPreview;

        fn preview(cfg: PreviewConfig) -> Self::Prev {
            let _ = cfg;
            StatefulAccountManagementPreview::new()
        }
    }
}
