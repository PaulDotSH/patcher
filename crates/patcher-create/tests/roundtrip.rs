//! End-to-end: generate a small corpus, create patches for every scenario, apply them to
//! a copy of the base game and compare against the modded folder byte for byte.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use patcher_core::format::{Footer, Meta};
use patcher_core::progress::Silent;
use patcher_core::{ApplyOptions, Error, GameState, Patch};
use patcher_create::{CreateOptions, Preset};
use patcher_testgen::{copy_tree, tree_listing, Corpus};

fn corpus() -> &'static (PathBuf, Corpus) {
    static C: OnceLock<(PathBuf, Corpus)> = OnceLock::new();
    C.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("patcher-test-{}", std::process::id()));
        let c = patcher_testgen::generate(&root.join("corpus"), 0.02, 42).unwrap();
        (root, c)
    })
}

fn opts(base: &Path, modded: &Path, preset: Preset, strong: bool) -> CreateOptions {
    CreateOptions {
        base: base.into(),
        modded: modded.into(),
        meta: Meta { name: "Test Mod".into(), version: "1.0".into(), ..Default::default() },
        profile: preset.profile(),
        strong_hashes: strong,
        excludes: vec![],
        ignore_files: vec![],
        max_mem: Some(2 << 30),
    }
}

fn fresh_game(tag: &str) -> PathBuf {
    let (root, c) = corpus();
    let game = root.join("games").join(tag);
    let _ = std::fs::remove_dir_all(&game);
    copy_tree(&c.base, &game).unwrap();
    game
}

fn roundtrip(scenario: usize, preset: Preset, strong: bool) {
    let (root, c) = corpus();
    let sc = &c.scenarios[scenario];
    let tag = format!("{}-{preset:?}-{strong}", sc.name);
    let patch_path = root.join(format!("{tag}.patch"));
    let report = patcher_create::create(&opts(&c.base, &sc.modded, preset, strong), &patch_path, &Silent).unwrap();
    assert!(report.patch_size > 0);

    let patch = Patch::open(&patch_path).unwrap();
    assert_eq!(patch.index.strong_hashes, strong);
    let game = fresh_game(&tag);
    let base_listing = tree_listing(&game).unwrap();
    assert_eq!(patcher_core::inspect(&patch, &game, &Silent).unwrap(), GameState::Unpatched);

    patcher_core::apply(&patch, &game, &ApplyOptions { backup: true, progress: &Silent }).unwrap();
    assert_eq!(tree_listing(&game).unwrap(), tree_listing(&sc.modded).unwrap(), "{tag}: patched game differs");
    assert_eq!(patcher_core::inspect(&patch, &game, &Silent).unwrap(), GameState::Patched);
    assert!(matches!(
        patcher_core::apply(&patch, &game, &ApplyOptions { backup: false, progress: &Silent }),
        Err(Error::AlreadyApplied)
    ));

    patcher_core::revert(&game, &Silent).unwrap();
    assert_eq!(tree_listing(&game).unwrap(), base_listing, "{tag}: revert differs from base");
    std::fs::remove_dir_all(&game).unwrap();
}

#[test]
fn every_scenario_balanced() {
    for i in 0..corpus().1.scenarios.len() {
        roundtrip(i, Preset::Balanced, true);
    }
}

#[test]
fn fast_and_max_without_strong_hashes() {
    roundtrip(4, Preset::Fast, false);
    roundtrip(4, Preset::Max, false);
}

#[test]
fn no_backup_and_copy_modes() {
    let (root, c) = corpus();
    let sc = &c.scenarios[4];
    let patch_path = root.join("modes.patch");
    patcher_create::create(&opts(&c.base, &sc.modded, Preset::Fast, true), &patch_path, &Silent).unwrap();
    let patch = Patch::open(&patch_path).unwrap();

    let game = fresh_game("no-backup");
    patcher_core::apply(&patch, &game, &ApplyOptions { backup: false, progress: &Silent }).unwrap();
    assert_eq!(tree_listing(&game).unwrap(), tree_listing(&sc.modded).unwrap());
    assert!(patcher_core::revert(&game, &Silent).is_err());

    let src = fresh_game("copy-src");
    let out = root.join("games").join("copy-out");
    let _ = std::fs::remove_dir_all(&out);
    patcher_core::apply_to_copy(&patch, &src, &out, &Silent).unwrap();
    assert_eq!(tree_listing(&out).unwrap(), tree_listing(&sc.modded).unwrap());
    assert_eq!(tree_listing(&src).unwrap(), tree_listing(&c.base).unwrap(), "source must stay untouched");
}

#[test]
fn rejects_wrong_base_without_touching_it() {
    let (root, c) = corpus();
    let sc = &c.scenarios[0];
    let patch_path = root.join("wrong-base.patch");
    patcher_create::create(&opts(&c.base, &sc.modded, Preset::Fast, false), &patch_path, &Silent).unwrap();
    let patch = Patch::open(&patch_path).unwrap();

    let game = fresh_game("wrong-base");
    let exe = game.join("Game.exe");
    let mut d = std::fs::read(&exe).unwrap();
    d[5000] ^= 1;
    std::fs::write(&exe, &d).unwrap();
    let before = tree_listing(&game).unwrap();
    match patcher_core::apply(&patch, &game, &ApplyOptions { backup: true, progress: &Silent }) {
        Err(Error::BaseMismatch(list)) => assert_eq!(list, vec!["changed: Game.exe".to_string()]),
        other => panic!("expected mismatch, got {other:?}"),
    }
    assert_eq!(tree_listing(&game).unwrap(), before);
    assert!(!game.join(".patcher-backup").exists() && !game.join(".patcher-staging").exists());
}

#[test]
fn standalone_patcher_carries_the_patch() {
    let (root, c) = corpus();
    let sc = &c.scenarios[1];
    let patch_path = root.join("standalone.patch");
    patcher_create::create(&opts(&c.base, &sc.modded, Preset::Fast, true), &patch_path, &Silent).unwrap();
    let exe = root.join("standalone.exe");
    let fake_stub = b"MZ fake stub with some bytes PTCHDB inside".repeat(1000);
    patcher_create::write_standalone(&fake_stub, &patch_path, &exe).unwrap();

    let bytes = std::fs::read(&exe).unwrap();
    let f = Footer::find(&bytes).unwrap();
    assert_eq!(f.patch_offset, fake_stub.len() as u64);
    let embedded = Patch::open_embedded(&exe).unwrap().expect("embedded patch");
    let plain = Patch::open(&patch_path).unwrap();
    assert_eq!(embedded.index, plain.index);
    assert_eq!(embedded.bytes(), plain.bytes());
    assert!(Patch::open_embedded(&patch_path).unwrap().is_none());

    let game = fresh_game("standalone");
    patcher_core::apply(&embedded, &game, &ApplyOptions { backup: true, progress: &Silent }).unwrap();
    assert_eq!(tree_listing(&game).unwrap(), tree_listing(&sc.modded).unwrap());
}

#[test]
fn moves_are_detected() {
    let (_, c) = corpus();
    let sc = &c.scenarios[3];
    let plan = patcher_create::plan(&opts(&c.base, &sc.modded, Preset::Fast, false), &Silent).unwrap();
    let summary = plan.summary();
    let count = |f: &dyn Fn(&patcher_create::Action) -> bool| summary.iter().filter(|(a, ..)| f(a)).count();
    assert!(count(&|a| matches!(a, patcher_create::Action::Copied { .. })) >= 3, "exact moves become copies");
    assert!(count(&|a| matches!(a, patcher_create::Action::Renamed { .. })) >= 3, "edited moves diff against the old path");
}

#[test]
fn excludes_are_honoured() {
    let (root, c) = corpus();
    let sc = &c.scenarios[3];
    let mut o = opts(&c.base, &sc.modded, Preset::Fast, false);
    o.excludes = vec!["mods/".into()];
    let patch_path = root.join("excl.patch");
    patcher_create::create(&o, &patch_path, &Silent).unwrap();
    let patch = Patch::open(&patch_path).unwrap();
    assert!(patch.index.writes.iter().all(|w| !w.path.starts_with("mods/")));
    assert!(patch.index.writes.iter().any(|w| w.path.starts_with("data/")));
}

#[test]
fn corrupt_patches_are_rejected() {
    let (root, c) = corpus();
    let sc = &c.scenarios[0];
    let patch_path = root.join("corrupt.patch");
    patcher_create::create(&opts(&c.base, &sc.modded, Preset::Fast, true), &patch_path, &Silent).unwrap();
    let good = std::fs::read(&patch_path).unwrap();
    for cut in [10, 100, good.len() / 2, good.len() - 3] {
        assert!(Patch::from_vec(good[..cut].to_vec()).is_err(), "truncated at {cut}");
    }
    // Flip a byte inside a blob: decoding or output verification must fail cleanly.
    let mut bad = good.clone();
    bad[60] ^= 0xFF;
    if let Ok(p) = Patch::from_vec(bad) {
        let game = fresh_game("corrupt");
        let before = tree_listing(&game).unwrap();
        assert!(patcher_core::apply(&p, &game, &ApplyOptions { backup: true, progress: &Silent }).is_err());
        assert_eq!(tree_listing(&game).unwrap(), before);
    }
}

/// Writes `files` (path, contents) under a fresh `root/hand/<tag>/<side>`.
fn tree(tag: &str, side: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let dir = corpus().0.join("hand").join(tag).join(side);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (p, d) in files {
        let p = dir.join(p);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, d).unwrap();
    }
    dir
}

/// Creates a patch from `base` to `modded` and returns it with a fresh copy of `base`.
fn hand_patch(tag: &str, base: &Path, modded: &Path) -> (Patch, PathBuf) {
    let path = base.parent().unwrap().join(format!("{tag}.patch"));
    patcher_create::create(&opts(base, modded, Preset::Fast, true), &path, &Silent).unwrap();
    let game = base.parent().unwrap().join(format!("game-{tag}"));
    let _ = std::fs::remove_dir_all(&game);
    copy_tree(base, &game).unwrap();
    (Patch::open(&path).unwrap(), game)
}

#[test]
fn case_only_renames_are_overwrites() {
    let hero_old = b"old hero pixels ".repeat(100);
    let hero_new = b"new hero pixels!".repeat(100);
    let base = tree("case", "base", &[("Data/Hero.png", &hero_old), ("Data/Same.txt", b"same"), ("Data/Gone.txt", b"gone")]);
    let modded = tree("case", "modded", &[("data/hero.png", &hero_new), ("data/same.txt", b"same")]);
    let plan = patcher_create::plan(&opts(&base, &modded, Preset::Fast, false), &Silent).unwrap();
    let mut summary: Vec<_> = plan.summary().into_iter().map(|(a, p, _)| (format!("{a:?}"), p)).collect();
    summary.sort();
    assert_eq!(
        summary,
        [("Deleted".to_string(), "Data/Gone.txt".to_string()), ("Modified".into(), "data/hero.png".into())]
    );

    for backup in [true, false] {
        let (patch, game) = hand_patch(&format!("case-{backup}"), &base, &modded);
        assert_eq!(patch.index.base_files.iter().filter(|b| b.path == "Data/Hero.png").count(), 1);
        patcher_core::apply(&patch, &game, &ApplyOptions { backup, progress: &Silent }).unwrap();
        assert_eq!(std::fs::read(game.join("Data").join("Hero.png")).unwrap(), hero_new);
        assert_eq!(std::fs::read(game.join("Data").join("Same.txt")).unwrap(), b"same");
        assert!(!game.join("Data").join("Gone.txt").exists());
        if backup {
            patcher_core::revert(&game, &Silent).unwrap();
            assert_eq!(tree_listing(&game).unwrap(), tree_listing(&base).unwrap());
        }
    }
}

#[test]
fn add_only_patch_is_detected_as_applied() {
    let base = tree("addonly", "base", &[("a.txt", b"a")]);
    let modded = tree("addonly", "modded", &[("a.txt", b"a"), ("mods/new.txt", b"new file")]);
    let (patch, game) = hand_patch("addonly", &base, &modded);
    assert_eq!(patcher_core::inspect(&patch, &game, &Silent).unwrap(), GameState::Unpatched);
    patcher_core::apply(&patch, &game, &ApplyOptions { backup: true, progress: &Silent }).unwrap();
    assert_eq!(patcher_core::inspect(&patch, &game, &Silent).unwrap(), GameState::Patched);
    assert!(matches!(
        patcher_core::apply(&patch, &game, &ApplyOptions { backup: false, progress: &Silent }),
        Err(Error::AlreadyApplied)
    ));
}

#[test]
fn files_and_directories_swap_places() {
    let base = tree("swap", "base", &[("foo/x.txt", b"x in dir foo"), ("bar", b"file bar"), ("keep.txt", b"k")]);
    let modded = tree("swap", "modded", &[("foo", b"file foo"), ("bar/y.txt", b"y in dir bar"), ("keep.txt", b"k")]);
    for backup in [true, false] {
        let (patch, game) = hand_patch(&format!("swap-{backup}"), &base, &modded);
        patcher_core::apply(&patch, &game, &ApplyOptions { backup, progress: &Silent }).unwrap();
        assert_eq!(tree_listing(&game).unwrap(), tree_listing(&modded).unwrap());
        if backup {
            patcher_core::revert(&game, &Silent).unwrap();
            assert_eq!(tree_listing(&game).unwrap(), tree_listing(&base).unwrap());
        }
    }
}

#[test]
fn copy_output_inside_the_game_is_rejected() {
    let base = tree("copyin", "base", &[("a.txt", b"a")]);
    let modded = tree("copyin", "modded", &[("a.txt", b"b")]);
    let (patch, game) = hand_patch("copyin", &base, &modded);
    for out in [game.join("out"), game.join("x").join("..").join("y"), game.clone()] {
        assert!(patcher_core::apply_to_copy(&patch, &game, &out, &Silent).is_err(), "{}", out.display());
    }
    assert_eq!(tree_listing(&game).unwrap(), tree_listing(&base).unwrap());
}

#[cfg(unix)]
#[test]
fn exec_bit_only_change_ships() {
    use std::os::unix::fs::PermissionsExt;
    let base = tree("exec", "base", &[("run.sh", b"echo hi\n")]);
    let modded = tree("exec", "modded", &[("run.sh", b"echo hi\n")]);
    std::fs::set_permissions(base.join("run.sh"), std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(modded.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let (patch, game) = hand_patch("exec", &base, &modded);
    assert_eq!(patch.index.writes.len(), 1);
    patcher_core::apply(&patch, &game, &ApplyOptions { backup: true, progress: &Silent }).unwrap();
    assert_eq!(std::fs::metadata(game.join("run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(patcher_core::inspect(&patch, &game, &Silent).unwrap(), GameState::Patched);
}

#[test]
fn hostile_payloads_fail_cleanly() {
    use patcher_core::codec::{bsdiff_apply, decompress};
    use patcher_core::format::Comp;
    use patcher_core::varint::put_u64;

    // Section lengths whose sum overflows.
    let mut p = Vec::new();
    for v in [1, u64::MAX / 2, u64::MAX / 2, 4] {
        put_u64(&mut p, v);
    }
    p.extend_from_slice(&[0; 8]);
    assert!(bsdiff_apply(b"old", &p, 16).is_err());
    // An absurd output length must not be allocated up front.
    let mut p = Vec::new();
    for v in [0, 0, 0, 0] {
        put_u64(&mut p, v);
    }
    assert!(bsdiff_apply(b"old", &p, usize::MAX / 2).is_err());

    // Output longer than declared, or a claimed size far beyond the data.
    let bomb = zstd::encode_all(&vec![0u8; 16 << 20][..], 3).unwrap();
    assert!(decompress(Comp::Zstd, &bomb, None, 1000).is_err());
    assert!(decompress(Comp::Zstd, &bomb, None, 1 << 40).is_err());
    assert!(decompress(Comp::Lzma2 { dict_size: u32::MAX }, &[0], None, 10).is_err());
    assert!(decompress(Comp::Lzma2 { dict_size: 1 << 20 }, &[0], None, usize::MAX).is_err());
}

#[test]
fn hostile_indexes_are_rejected() {
    use patcher_core::format::{
        BaseFile, Blob, Check, Codec, Header, Index, Segment, Source, Transform, WriteOp, FORMAT_VERSION, HEADER_LEN,
    };
    let bytes = |idx: &Index| {
        let raw = idx.encode();
        let comp = zstd::encode_all(&raw[..], 3).unwrap();
        let data = [7u8; 64];
        let index_offset = (HEADER_LEN + data.len()) as u64;
        let h = Header {
            version: FORMAT_VERSION,
            flags: 0,
            index_offset,
            index_len: comp.len() as u64,
            index_raw_len: raw.len() as u64,
            total_len: index_offset + comp.len() as u64,
        };
        [&h.to_bytes()[..], &data, &comp].concat()
    };
    let check = |size| Check { size, xxh3: 0, blake3: None };
    let seg = |len| Segment {
        len,
        blob: Blob { offset: HEADER_LEN as u64, len: 8 },
        codec: Codec::Raw,
        transform: Transform::None,
        base: None,
    };
    let write = |path: &str, segs: Vec<Segment>| WriteOp {
        path: path.into(),
        check: check(segs.iter().map(|s| s.len).fold(0, u64::wrapping_add)),
        exec: false,
        source: Source::Segments(segs),
    };
    let ok = Index { writes: vec![write("a.txt", vec![seg(8)])], ..Default::default() };
    Patch::from_vec(bytes(&ok)).unwrap();

    let cases = [
        vec![write(".patcher-backup/x", vec![seg(8)])],
        vec![write(".Patcher-Staging/x", vec![seg(8)])],
        vec![write("a.txt", vec![seg(8)]), write("A.TXT", vec![seg(8)])],
        vec![write("big", vec![seg(1 << 40)])],
        vec![write("wrap", vec![seg(u64::MAX), seg(9)])],
    ];
    for writes in cases {
        let idx = Index { writes, ..Default::default() };
        assert!(Patch::from_vec(bytes(&idx)).is_err(), "{:?}", idx.writes[0].path);
    }
    let both = Index {
        base_files: vec![BaseFile { path: "A.txt".into(), check: check(1) }],
        writes: vec![write("a.txt", vec![seg(8)])],
        deletes: vec![0],
        ..Default::default()
    };
    assert!(Patch::from_vec(bytes(&both)).is_err());
}
