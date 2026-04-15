#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseTag {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl ReleaseTag {
    pub fn core(self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }

    pub fn next_patch_core(self) -> Option<String> {
        let next_patch = self.patch.checked_add(1)?;
        Some(format!("{}.{}.{}", self.major, self.minor, next_patch))
    }
}

pub fn parse_release_tag(tag: &str) -> Option<ReleaseTag> {
    let trimmed = tag.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed.strip_prefix('v').unwrap_or(trimmed);
    if normalized.contains('-') || normalized.contains('+') {
        return None;
    }

    let mut parts = normalized.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }

    Some(ReleaseTag {
        major,
        minor,
        patch,
    })
}

pub fn version_from_git_describe(describe: &str, dirty: bool) -> Option<String> {
    let describe = describe.trim();
    let mut fields = describe.splitn(3, '-');
    let tag = parse_release_tag(fields.next()?)?;
    let commits_since_tag = fields.next()?.parse::<u64>().ok()?;
    let short_sha = fields.next()?;
    let short_sha_hex = short_sha.strip_prefix('g')?;
    if short_sha_hex.is_empty() || !short_sha_hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }

    let mut version = if commits_since_tag == 0 {
        tag.core()
    } else {
        let next_core = tag.next_patch_core()?;
        format!("{next_core}-dev.{commits_since_tag}+{short_sha}")
    };

    if dirty {
        append_dirty_metadata(&mut version);
    }

    Some(version)
}

pub fn fallback_version(package_version: &str, short_sha: Option<&str>, dirty: bool) -> String {
    let mut version = package_version.trim().to_string();

    if let Some(short_sha) = short_sha {
        let identifier = format!("g{short_sha}");
        append_build_metadata(&mut version, &identifier);
    }

    if dirty {
        append_dirty_metadata(&mut version);
    }

    version
}

pub fn append_dirty_metadata(version: &mut String) {
    append_build_metadata(version, "dirty");
}

fn append_build_metadata(version: &mut String, identifier: &str) {
    if identifier.is_empty() {
        return;
    }

    if version.contains('+') {
        version.push('.');
    } else {
        version.push('+');
    }
    version.push_str(identifier);
}
