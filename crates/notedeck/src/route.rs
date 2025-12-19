#[derive(Clone, Debug, Default)]
pub struct DrawerRouter {
    pub returning: bool,
    pub navigating: bool,
    pub drawer_focused: bool,
}

impl DrawerRouter {
    pub fn open(&mut self) {
        self.navigating = true;
    }

    pub fn close(&mut self) {
        self.returning = true;
    }

    pub fn closed(&mut self) {
        self.clear();
        self.drawer_focused = false;
    }

    fn clear(&mut self) {
        self.navigating = false;
        self.returning = false;
    }

    pub fn opened(&mut self) {
        self.clear();
        self.drawer_focused = true;
    }
}

#[derive(Clone, Debug)]
pub struct Router<R: Clone> {
    pub routes: Vec<R>,
    pub returning: bool,
    pub navigating: bool,
    replacing: Option<ReplacementType>,
}

#[derive(Clone, Debug)]
pub enum ReplacementType {
    Single,
    All,
}

impl<R: Clone> Router<R> {
    pub fn new(routes: Vec<R>) -> Self {
        if routes.is_empty() {
            panic!("routes can't be empty")
        }
        let returning = false;
        let navigating = false;
        let replacing = None;

        Self {
            routes,
            returning,
            replacing,
            navigating,
        }
    }

    pub fn route_to(&mut self, route: R) {
        self.navigating = true;
        self.routes.push(route);
    }

    // Route to R. Then when it is successfully placed, should call `remove_previous_routes` to remove all previous routes
    pub fn route_to_replaced(&mut self, route: R, replacement_type: ReplacementType) {
        self.replacing = Some(replacement_type);
        self.route_to(route);
    }

    /// Go back, start the returning process
    pub fn go_back(&mut self) -> Option<R> {
        if self.returning || self.routes.len() == 1 {
            return None;
        }
        self.returning = true;

        if self.routes.len() == 1 {
            return None;
        }

        self.prev().cloned()
    }

    pub fn pop(&mut self) -> Option<R> {
        if self.routes.len() == 1 {
            return None;
        }

        self.returning = false;
        self.routes.pop()
    }

    pub fn top(&self) -> &R {
        self.routes.last().expect("routes can't be empty")
    }

    pub fn prev(&self) -> Option<&R> {
        self.routes.get(self.routes.len() - 2)
    }

    pub fn routes(&self) -> &Vec<R> {
        &self.routes
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn is_replacing(&self) -> bool {
        self.replacing.is_some()
    }

    pub fn complete_replacement(&mut self) {
        let num_routes = self.len();

        self.returning = false;
        let Some(replacement) = self.replacing.take() else {
            return;
        };
        if num_routes < 2 {
            return;
        }

        match replacement {
            ReplacementType::Single => {
                self.routes.remove(num_routes - 2);
            }
            ReplacementType::All => {
                self.routes.drain(..num_routes - 1);
            }
        }
    }
}
