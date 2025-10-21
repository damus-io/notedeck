fn main() {
    robius_open::Uri::new("tel:+61 0400 000 000")
        .action("ACTION_DIAL")
        .open()
        .unwrap();
}
