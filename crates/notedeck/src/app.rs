use crate::AppContext;

pub trait App {
    fn update(&mut self, ctx: &mut AppContext<'_>);
}
