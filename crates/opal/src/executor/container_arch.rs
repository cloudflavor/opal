use std::env;

pub(crate) fn default_container_cli_arch(platform: Option<&str>) -> Option<String> {
    container_arch_override()
        .or_else(|| platform.and_then(container_arch_from_platform))
        .or_else(host_container_arch)
}

fn container_arch_override() -> Option<String> {
    env::var("OPAL_CONTAINER_ARCH")
        .ok()
        .filter(|value| !value.is_empty())
}

fn host_container_arch() -> Option<String> {
    normalize_container_arch(std::env::consts::ARCH)
}

pub(crate) fn normalize_container_arch(value: &str) -> Option<String> {
    match value {
        "aarch64" => Some("arm64".to_string()),
        "x86_64" => Some("x86_64".to_string()),
        other if !other.is_empty() => Some(other.to_string()),
        _ => None,
    }
}

pub(crate) fn container_arch_from_platform(value: &str) -> Option<String> {
    let normalized = value.to_ascii_lowercase();
    if normalized.contains("amd64") || normalized.contains("x86_64") {
        return Some("x86_64".to_string());
    }
    if normalized.contains("arm64") || normalized.contains("aarch64") {
        return Some("arm64".to_string());
    }
    None
}
