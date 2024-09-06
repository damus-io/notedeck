#[derive(Default)]
pub struct RoutableWidgetState<R: Clone> {
    routes: Vec<R>,
}

impl<R: Clone> RoutableWidgetState<R> {
    pub fn route_to(&mut self, route: R) {
        self.routes.push(route);
    }

    pub fn clear(&mut self) {
        self.routes.clear();
    }

    pub fn go_back(&mut self) {
        self.routes.pop();
    }

    pub fn top(&self) -> Option<R> {
        self.routes.last().cloned()
    }

    pub fn get_routes(&self) -> Vec<R> {
        self.routes.clone()
    }
}
