fn main() {
    robius_open::Uri::new("mailto:test@example.com")
        .action("ACTION_MAIL")
        .open()
        .unwrap();
}
