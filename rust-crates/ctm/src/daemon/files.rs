//! Document upload/download handling functions and file utilities.

use super::*;

/// Image file extensions that should be sent as photos (with inline preview).
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp"];

/// Maximum file size for Telegram uploads (50 MB).
const MAX_UPLOAD_BYTES: u64 = 50 * 1024 * 1024;

/// Ensure downloads directory exists with restrictive permissions.
pub(super) fn ensure_downloads_dir() -> PathBuf {
    let dir = get_config_dir().join("downloads");
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    dir
}

/// Sanitize filename to prevent path traversal.
pub(super) fn sanitize_filename(name: &str) -> String {
    let mut safe = name.replace(['/', '\\'], "_");
    safe = safe.replace("..", "_");
    if safe.starts_with('.') {
        safe = format!("_{safe}");
    }
    // U-2: Char-safe truncation — avoid panicking on multibyte UTF-8 characters.
    if safe.chars().count() > 200 {
        safe = safe.chars().take(200).collect();
    }
    format!("{}_{safe}", uuid::Uuid::new_v4())
}

/// Validate a file path for send_image.
pub(super) fn validate_send_image_path(
    path: &str,
) -> std::result::Result<&std::path::Path, &'static str> {
    let p = std::path::Path::new(path);
    if !p.is_absolute() {
        return Err("path must be absolute");
    }
    if path.contains("..") {
        return Err("path must not contain '..'");
    }
    if !p.exists() {
        return Err("file does not exist");
    }
    match p.metadata() {
        Ok(meta) => {
            if meta.len() > MAX_UPLOAD_BYTES {
                return Err("file exceeds 50 MB limit");
            }
        }
        Err(_) => return Err("cannot read file metadata"),
    }
    Ok(p)
}

/// Check if a file extension indicates an image (for sendPhoto vs sendDocument).
pub(super) fn is_image_extension(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename() {
        let name = sanitize_filename("photo_123.jpg");
        assert!(name.ends_with("_photo_123.jpg"));
        assert!(!name.contains('/'));

        let traversal = sanitize_filename("../../../etc/passwd");
        assert!(!traversal.contains(".."));

        let dotfile = sanitize_filename(".hidden");
        assert!(!dotfile.starts_with('.'));
    }

    #[test]
    fn test_validate_send_image_path_relative() {
        assert!(validate_send_image_path("relative/path.png").is_err());
    }

    #[test]
    fn test_validate_send_image_path_traversal() {
        assert!(validate_send_image_path("/tmp/../etc/passwd").is_err());
    }

    #[test]
    fn test_validate_send_image_path_nonexistent() {
        assert!(validate_send_image_path("/tmp/definitely_nonexistent_file_xyzzy.png").is_err());
    }

    #[test]
    fn test_is_image_extension() {
        use std::path::Path;
        assert!(is_image_extension(Path::new("photo.jpg")));
        assert!(is_image_extension(Path::new("photo.JPEG")));
        assert!(is_image_extension(Path::new("photo.png")));
        assert!(is_image_extension(Path::new("photo.gif")));
        assert!(is_image_extension(Path::new("photo.webp")));
        assert!(is_image_extension(Path::new("photo.bmp")));
        assert!(!is_image_extension(Path::new("doc.pdf")));
        assert!(!is_image_extension(Path::new("file.txt")));
        assert!(!is_image_extension(Path::new("archive.zip")));
        assert!(!is_image_extension(Path::new("noext")));
    }
}
