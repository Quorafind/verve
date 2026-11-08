//! The archive reader on its own: the format, the tree, links, unpacked files, integrity.

use sha2::{Digest, Sha256};
use verve::encoding::hex;
use verve::vfs::Kind;

use crate::archive::{Archive, AsarError, Entry, Place};
use crate::overlay::AsarOptions;

#[path = "../tests/support/mod.rs"]
mod support;

use support::{Dir, Pack, raw};

fn options(validate_integrity: bool) -> AsarOptions {
    AsarOptions {
        validate_integrity,
        ..AsarOptions::default()
    }
}

fn message(error: &std::io::Error) -> String {
    AsarError::of(error).map_or_else(|| error.to_string(), |e| e.message().to_owned())
}

#[test]
fn nested_files_are_read_from_where_the_header_puts_them() {
    let dir = Dir::new("nested");
    let at = dir.at("app.asar");
    Pack::new()
        .file("package.json", b"{\"main\":\"main.js\"}")
        .file("dist/main.js", b"console.log(1)")
        .file("dist/deep/er/x.txt", b"deep")
        .file("empty.txt", b"")
        .write(&at);
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert_eq!(
        archive.read("package.json").unwrap(),
        b"{\"main\":\"main.js\"}"
    );
    assert_eq!(archive.read("dist/main.js").unwrap(), b"console.log(1)");
    assert_eq!(archive.read("dist/deep/er/x.txt").unwrap(), b"deep");
    assert_eq!(archive.read("empty.txt").unwrap(), b"");
    // The same path, written any way a caller might.
    assert_eq!(
        archive.read("dist/./deep//er/../er/x.txt").unwrap(),
        b"deep"
    );
}

#[test]
fn a_folder_lists_its_names_with_their_kinds() {
    let dir = Dir::new("list");
    let at = dir.at("app.asar");
    Pack::new()
        .file("b.txt", b"b")
        .file("a/inner.txt", b"i")
        .link("l", "b.txt")
        .write(&at);
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert_eq!(
        archive.list("").unwrap(),
        [
            ("a".to_owned(), Kind::Dir),
            ("b.txt".to_owned(), Kind::File),
            ("l".to_owned(), Kind::Symlink),
        ]
    );
    assert_eq!(
        archive.list("a").unwrap(),
        [("inner.txt".to_owned(), Kind::File)]
    );
}

#[test]
fn missing_entries_and_files_read_as_folders_fail_in_electrons_words() {
    let dir = Dir::new("errors");
    let at = dir.at("app.asar");
    Pack::new().file("a.txt", b"a").write(&at);
    let archive = Archive::open(&at, &options(false)).unwrap();
    let missing = archive.read("nope/x.txt").unwrap_err();
    assert_eq!(
        message(&missing),
        format!("ENOENT, nope/x.txt not found in {}", at.display())
    );
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    let through_a_file = archive.read("a.txt/x").unwrap_err();
    assert_eq!(through_a_file.kind(), std::io::ErrorKind::NotFound);
    let not_dir = archive.list("a.txt").unwrap_err();
    assert_eq!(message(&not_dir), "ENOTDIR, not a directory");
    assert_eq!(not_dir.kind(), std::io::ErrorKind::NotADirectory);
    let dir_read = archive.read("").unwrap_err();
    assert_eq!(dir_read.kind(), std::io::ErrorKind::IsADirectory);
}

#[test]
fn links_are_followed_on_the_way_and_at_the_end_when_asked() {
    let dir = Dir::new("links");
    let at = dir.at("app.asar");
    Pack::new()
        .file("lib/main.js", b"main")
        .link("latest", "lib/main.js")
        .link("libs", "lib")
        .link("again", "latest")
        .link("loop-a", "loop-b")
        .link("loop-b", "loop-a")
        .link("up", "lib/../../out")
        .write(&at);
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert_eq!(archive.read("latest").unwrap(), b"main");
    assert_eq!(archive.read("libs/main.js").unwrap(), b"main");
    assert_eq!(archive.read("again").unwrap(), b"main");
    assert_eq!(archive.resolve("again", true).unwrap().path, "lib/main.js");
    assert_eq!(
        archive.resolve("libs/main.js", true).unwrap().path,
        "lib/main.js"
    );
    // Not followed at the end, a link is the link.
    let link = archive.resolve("latest", false).unwrap();
    assert!(matches!(link.entry, Entry::Link(target) if target == "lib/main.js"));
    // A loop and a link out of the root are not found.
    assert_eq!(
        archive.read("loop-a").unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(
        archive.read("up").unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn unpacked_files_are_the_real_ones_beside_the_archive() {
    let dir = Dir::new("unpacked");
    let at = dir.at("app.asar");
    Pack::new()
        .file("a.txt", b"packed")
        .unpacked("native/addon.node", b"MZ-not-really")
        .write(&at);
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert_eq!(archive.read("native/addon.node").unwrap(), b"MZ-not-really");
    let found = archive.resolve("native/addon.node", true).unwrap();
    let Entry::File(file) = found.entry else {
        panic!("a file");
    };
    assert!(matches!(file.place, Place::Unpacked));
    assert_eq!(file.size, 13);
    assert_eq!(
        archive.unpacked_path("native/addon.node"),
        dir.at("app.asar.unpacked")
            .join("native")
            .join("addon.node")
    );
    // The folder its file is in lists as the archive lists it.
    assert_eq!(
        archive.list("native").unwrap(),
        [("addon.node".to_owned(), Kind::File)]
    );
}

#[test]
fn a_folder_marked_unpacked_unpacks_what_is_in_it() {
    let dir = Dir::new("unpacked-dir");
    let at = dir.at("app.asar");
    let json = br#"{"files":{"bin":{"unpacked":true,"files":{"tool":{"size":4}}}}}"#;
    std::fs::write(&at, raw(json, b"")).unwrap();
    std::fs::create_dir_all(dir.at("app.asar.unpacked/bin")).unwrap();
    std::fs::write(dir.at("app.asar.unpacked/bin/tool"), b"tool").unwrap();
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert_eq!(archive.read("bin/tool").unwrap(), b"tool");
}

#[test]
fn executable_files_say_so() {
    let dir = Dir::new("executable");
    let at = dir.at("app.asar");
    Pack::new()
        .executable("run.sh", b"#!/bin/sh")
        .file("a", b"a")
        .write(&at);
    let archive = Archive::open(&at, &options(false)).unwrap();
    let executable = |inner: &str| match archive.resolve(inner, true).unwrap().entry {
        Entry::File(file) => file.executable,
        _ => panic!("a file"),
    };
    assert!(executable("run.sh"));
    assert!(!executable("a"));
}

#[test]
fn a_file_that_is_not_an_archive_is_an_invalid_package() {
    let dir = Dir::new("invalid");
    let cases: [(&str, Vec<u8>); 5] = [
        ("empty", Vec::new()),
        ("text", b"this is not an archive at all".to_vec()),
        ("json", raw(b"not json", b"")),
        ("no-files", raw(b"{}", b"")),
        ("cut", raw(br#"{"files":{}}"#, b"")[..14].to_vec()),
    ];
    for (name, bytes) in cases {
        let at = dir.at(&format!("{name}.asar"));
        std::fs::write(&at, bytes).unwrap();
        let error = Archive::open(&at, &options(false)).err().unwrap();
        assert_eq!(
            message(&error),
            format!("Invalid package {}", at.display()),
            "{name}"
        );
    }
    // A header that claims more than Electron's reader accepts.
    let at = dir.at("huge.asar");
    let mut huge = raw(br#"{"files":{}}"#, b"");
    huge[4..8].copy_from_slice(&(300u32 << 20).to_le_bytes());
    std::fs::write(&at, huge).unwrap();
    assert!(Archive::open(&at, &options(false)).is_err());
}

#[test]
fn an_offset_past_two_to_the_fifty_third_is_read_exactly_and_refused_as_out_of_the_archive() {
    let dir = Dir::new("offset");
    let at = dir.at("app.asar");
    // 2^53 + 1 is not a number JSON-as-doubles can hold; as a string it is exact.
    let json = br#"{"files":{"far":{"size":1,"offset":"9007199254740993"},"near":{"size":2,"offset":"0"}}}"#;
    std::fs::write(&at, raw(json, b"ok")).unwrap();
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert_eq!(archive.read("near").unwrap(), b"ok");
    let error = archive.read("far").unwrap_err();
    assert_eq!(message(&error), format!("Invalid package {}", at.display()));
}

#[test]
fn integrity_is_checked_block_by_block_only_when_asked() {
    let dir = Dir::new("integrity");
    let at = dir.at("app.asar");
    Pack::new()
        .with_integrity(4)
        .file("a.txt", b"0123456789")
        .file("empty.txt", b"")
        .write(&at);
    let archive = Archive::open(&at, &options(true)).unwrap();
    assert_eq!(archive.read("a.txt").unwrap(), b"0123456789");
    assert_eq!(archive.read("empty.txt").unwrap(), b"");

    // One byte of the last block changes on the disk.
    let mut bytes = std::fs::read(&at).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&at, bytes).unwrap();
    let checked = Archive::open(&at, &options(true)).unwrap();
    let error = checked.read("a.txt").unwrap_err();
    assert!(AsarError::of(&error).unwrap().is_integrity());
    assert!(message(&error).starts_with("Integrity check failed for asar archive"));
    let unchecked = Archive::open(&at, &options(false)).unwrap();
    assert!(unchecked.read("a.txt").is_ok());
}

#[test]
fn the_header_hash_is_the_sha256_of_the_json() {
    let dir = Dir::new("hash");
    let at = dir.at("app.asar");
    Pack::new().file("a.txt", b"a").write(&at);
    let bytes = std::fs::read(&at).unwrap();
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let expected = hex(&Sha256::digest(&bytes[16..16 + json_len]));
    assert_eq!(
        Archive::open(&at, &options(false))
            .unwrap()
            .header_hash()
            .unwrap(),
        expected
    );
}

#[test]
#[ignore = "wall-clock bound; run with --ignored on a quiet machine"]
fn a_header_of_ten_thousand_files_opens_quickly() {
    let dir = Dir::new("large");
    let at = dir.at("app.asar");
    let mut pack = Pack::new();
    for n in 0..10_000 {
        pack = pack.file(&format!("node_modules/pkg{}/lib/file{n}.js", n % 100), b"x");
    }
    pack.write(&at);
    let started = std::time::Instant::now();
    let archive = Archive::open(&at, &options(false)).unwrap();
    // The brief's bound is 20 ms for a release build; a debug build gets room.
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    assert_eq!(
        archive.read("node_modules/pkg7/lib/file107.js").unwrap(),
        b"x"
    );
}

#[test]
fn only_the_whole_files_digest_is_compared() {
    let dir = Dir::new("whole-digest");
    let at = dir.at("app.asar");
    // A header as `@electron/asar` writes it, with block digests nothing here needs.
    let digest = hex(&Sha256::digest(b"abc"));
    let json = format!(
        r#"{{"files":{{"a":{{"size":3,"offset":"0","integrity":{{"algorithm":"SHA256","hash":"{}","blockSize":0,"blocks":[]}}}},"b":{{"size":3,"offset":"0","integrity":{{"algorithm":"SHA256","hash":"{}"}}}}}}}}"#,
        digest.to_uppercase(),
        "0".repeat(64)
    );
    std::fs::write(&at, raw(json.as_bytes(), b"abc")).unwrap();
    let archive = Archive::open(&at, &options(true)).unwrap();
    assert_eq!(archive.read("a").unwrap(), b"abc");
    let error = archive.read("b").unwrap_err();
    assert!(AsarError::of(&error).unwrap().is_integrity());
}

#[test]
fn a_header_that_lies_about_where_a_file_is_is_refused_not_trusted() {
    let dir = Dir::new("lies");
    let at = dir.at("app.asar");
    let json = br#"{"files":{
        "max":{"size":1,"offset":"18446744073709551615"},
        "wrap":{"size":18446744073709551615,"offset":"1"},
        "past":{"size":100,"offset":"0"},
        "huge":{"size":18446744073709551615,"offset":"0"}}}"#;
    std::fs::write(&at, raw(json, b"ok")).unwrap();
    let archive = Archive::open(&at, &options(false)).unwrap();
    for name in ["max", "wrap", "past", "huge"] {
        let error = archive.read(name).unwrap_err();
        assert_eq!(
            message(&error),
            format!("Invalid package {}", at.display()),
            "{name}"
        );
    }
}

#[test]
fn a_header_nested_past_the_parsers_limit_is_an_invalid_package() {
    let dir = Dir::new("deep");
    let at = dir.at("app.asar");
    let depth = 200;
    let json = format!(
        "{}{}",
        r#"{"files":{"d":"#.repeat(depth),
        "{}".to_owned() + &"}}".repeat(depth)
    );
    std::fs::write(&at, raw(json.as_bytes(), b"")).unwrap();
    let error = Archive::open(&at, &options(false)).err().unwrap();
    assert_eq!(message(&error), format!("Invalid package {}", at.display()));
}

#[test]
fn names_that_are_paths_are_left_out_of_their_folder() {
    let dir = Dir::new("names");
    let at = dir.at("app.asar");
    let json = br#"{"files":{"ok":{"size":0,"offset":"0"},"a/b":{"size":0,"offset":"0"},
        "x\\..\\f":{"size":0,"offset":"0"},"..":{"size":0,"offset":"0"},".":{"size":0,"offset":"0"}}}"#;
    std::fs::write(&at, raw(json, b"")).unwrap();
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert_eq!(archive.list("").unwrap(), [("ok".to_owned(), Kind::File)]);
}

#[cfg(windows)]
#[test]
fn a_link_written_with_backslashes_is_followed_on_windows() {
    let dir = Dir::new("backslash-link");
    let at = dir.at("app.asar");
    Pack::new()
        .file("lib/main.js", b"main")
        .link("latest", "lib\\main.js")
        .write(&at);
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert_eq!(archive.read("latest").unwrap(), b"main");
}

#[test]
fn the_real_path_is_found_once_when_the_archive_is_opened() {
    let dir = Dir::new("real-path");
    let at = dir.at("app.asar");
    Pack::new().file("a", b"a").write(&at);
    let archive = Archive::open(&at, &options(false)).unwrap();
    assert!(archive.real_path().ends_with("app.asar"));
}
