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
}

impl<R: Clone> Router<R> {
    pub fn new(routes: Vec<R>) -> Self {
        if routes.is_empty() {
            panic!("routes can't be empty")
        }
        let returning = false;
        let navigating = false;

        Self {
            routes,
            returning,
            navigating,
        }
    }

    pub fn route_to(&mut self, route: R) {
        self.navigating = true;
        self.routes.push(route);
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
}
