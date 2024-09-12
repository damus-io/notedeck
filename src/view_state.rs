use crate::login_manager::LoginState;

/// Various state for views
#[derive(Default)]
pub struct ViewState {
    pub login: LoginState,
}

impl ViewState {
    pub fn login_mut(&mut self) -> &mut LoginState {
        &mut self.login
    }
}
