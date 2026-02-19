use std::path::Path;

/// Abbreviate a path by replacing the given home directory prefix with ~
pub fn abbreviate_with_home(path: &Path, home_dir: &str) -> String {
    let home = Path::new(home_dir);
    if let Ok(relative) = path.strip_prefix(home) {
        return format!("~/{}", relative.display());
    }
    path.display().to_string()
}

/// Abbreviate a path using the local machine's home directory
pub fn abbreviate_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        abbreviate_with_home(path, &home.to_string_lossy())
    } else {
        path.display().to_string()
    }
}
