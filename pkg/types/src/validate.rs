use anyhow::{Result, bail};

/// Validate a Kubernetes-style resource name.
/// Rules: lowercase `[a-z0-9-]`, max 63 chars, no leading/trailing hyphens.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name must not be empty");
    }
    if name.len() > 63 {
        bail!("name '{}' exceeds 63 characters (got {})", name, name.len());
    }
    if name.starts_with('-') || name.ends_with('-') {
        bail!("name '{}' must not start or end with a hyphen", name);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!(
            "name '{}' must contain only lowercase letters, digits, and hyphens [a-z0-9-]",
            name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(validate_name("nginx").is_ok());
        assert!(validate_name("my-app").is_ok());
        assert!(validate_name("app-123").is_ok());
        assert!(validate_name("a").is_ok());
        assert!(validate_name("a-b-c-d").is_ok());
    }

    #[test]
    fn invalid_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name("My-App").is_err());
        assert!(validate_name("my_app").is_err());
        assert!(validate_name("-leading").is_err());
        assert!(validate_name("trailing-").is_err());
        assert!(validate_name("special!char").is_err());
        assert!(validate_name(&"a".repeat(64)).is_err());
    }
}
