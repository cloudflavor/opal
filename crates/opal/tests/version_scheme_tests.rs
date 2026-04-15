#[path = "../src/version_scheme.rs"]
mod version_scheme;

use version_scheme::{
    append_dirty_metadata, fallback_version, parse_release_tag, version_from_git_describe,
};

#[test]
fn parses_release_tags_with_optional_v_prefix() {
    let tag = parse_release_tag("v1.4.2").expect("tag should parse");
    assert_eq!(tag.core(), "1.4.2");

    let tag_without_prefix = parse_release_tag("2.3.4").expect("tag should parse");
    assert_eq!(tag_without_prefix.core(), "2.3.4");
}

#[test]
fn rejects_non_release_tags() {
    assert!(parse_release_tag("v1.4.2-rc1").is_none());
    assert!(parse_release_tag("release-1.4.2").is_none());
    assert!(parse_release_tag("v1.4").is_none());
}

#[test]
fn derives_release_version_on_tag() {
    let version = version_from_git_describe("v1.4.2-0-gabc1234", false).expect("version");
    assert_eq!(version, "1.4.2");
}

#[test]
fn derives_next_patch_dev_version_off_tag() {
    let version = version_from_git_describe("v1.4.2-7-gabc1234", false).expect("version");
    assert_eq!(version, "1.4.3-dev.7+gabc1234");
}

#[test]
fn adds_dirty_metadata_to_dev_version() {
    let version = version_from_git_describe("v1.4.2-7-gabc1234", true).expect("version");
    assert_eq!(version, "1.4.3-dev.7+gabc1234.dirty");
}

#[test]
fn adds_dirty_metadata_to_release_version() {
    let version = version_from_git_describe("v1.4.2-0-gabc1234", true).expect("version");
    assert_eq!(version, "1.4.2+dirty");
}

#[test]
fn fallback_version_appends_sha_and_dirty_metadata() {
    let version = fallback_version("0.1.0-rc8", Some("abc1234"), true);
    assert_eq!(version, "0.1.0-rc8+gabc1234.dirty");
}

#[test]
fn dirty_metadata_appends_to_existing_build_metadata() {
    let mut version = String::from("1.4.3-dev.7+gabc1234");
    append_dirty_metadata(&mut version);
    assert_eq!(version, "1.4.3-dev.7+gabc1234.dirty");
}
