use damus::WinitEvent;

fn main() {
    #[cfg(debug_assertions)]
    simple_logger::init().unwrap();
    let event_loop = winit::event_loop::EventLoopBuilder::<WinitEvent>::with_user_event().build();
    damus::main(event_loop);
}
