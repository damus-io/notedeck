fn main() {
    robius_open::Uri::new("https://github.com/project-robius")
        .action("ACTION_VIEW")
        .open()
        .unwrap();
}
