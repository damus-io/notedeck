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
