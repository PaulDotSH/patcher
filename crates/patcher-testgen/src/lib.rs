//! Deterministic synthetic "game" corpus and mod scenarios for tests and benchmarks.
//!
//! The base game imitates a typical small PC game: an x86-64 executable whose CALLs are
//! real rel32 references between functions, a PopCap-style `main.pak` (an XOR-0xF7
//! archive mixing text and incompressible images), compressed audio, XML configs, level
//! files and thousands of small assets. Each scenario derives a modded copy the way real
//! mods do: byte patches, relinked code, a repacked archive, added/renamed/deleted files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// ---- randomness and content ------------------------------------------------------

#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E3779B97F4A7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }

    /// Incompressible bytes (stand-in for PNG/OGG/JPEG payloads).
    pub fn noise(&mut self, n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n + 8);
        while v.len() < n {
            v.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        v.truncate(n);
        v
    }
}

const WORDS: &[&str] = &[
    "the", "zombie", "plant", "sun", "pea", "shooter", "wall", "nut", "lawn", "mower", "wave", "level", "flag",
    "garden", "night", "pool", "fog", "roof", "seed", "packet", "cooldown", "damage", "health", "speed", "armor",
    "cone", "bucket", "door", "screen", "pole", "vault", "dancer", "backup", "snorkel", "zomboni", "bobsled",
    "dolphin", "rider", "jack", "box", "balloon", "digger", "pogo", "yeti", "bungee", "ladder", "catapult",
    "gargantuar", "imp", "boss", "sunflower", "cherry", "bomb", "potato", "mine", "snow", "chomper", "repeater",
    "puff", "shroom", "fume", "grave", "buster", "hypno", "scaredy", "ice", "doom", "lily", "pad", "squash",
    "threepeater", "tangle", "kelp", "jalapeno", "spikeweed", "torchwood", "tall", "sea", "plantern", "cactus",
    "blover", "split", "starfruit", "pumpkin", "magnet", "cabbage", "pult", "flower", "pot", "kernel", "coffee",
    "bean", "garlic", "umbrella", "leaf", "marigold", "melon", "gatling", "twin", "gloom", "cattail", "winter",
    "gold", "magnet", "spikerock", "cob", "cannon", "imitater", "and", "of", "to", "a", "in", "is", "on", "with",
    "when", "it", "its", "this", "that", "for", "your", "you", "can", "will", "be", "are", "from", "at", "by",
];

impl Rng {
    pub fn word(&mut self) -> &'static str {
        // Zipf-ish: low indices are much more common.
        let r = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        WORDS[((r * r) * WORDS.len() as f64) as usize % WORDS.len()]
    }

    pub fn sentence(&mut self) -> String {
        let mut s = String::new();
        for i in 0..self.range(4, 14) {
            let w = self.word();
            if i == 0 {
                s += &w[..1].to_uppercase();
                s += &w[1..];
            } else {
                s.push(' ');
                s += w;
            }
        }
        s.push('.');
        s
    }

    pub fn text(&mut self, n: usize) -> Vec<u8> {
        let mut s = String::new();
        while s.len() < n {
            s += &self.sentence();
            s.push(if self.chance(0.2) { '\n' } else { ' ' });
        }
        s.truncate(n);
        s.into_bytes()
    }

    /// XML-ish animation/config data: very compressible, number-heavy.
    pub fn xml(&mut self, n: usize) -> Vec<u8> {
        let mut s = String::from("<?xml version=\"1.0\"?>\n<data>\n");
        while s.len() < n {
            let tag = self.word();
            s += &format!(
                "  <{tag} id=\"{}\" x=\"{:.1}\" y=\"{:.1}\" sx=\"{:.3}\" kx=\"{}\" f=\"{}\"/>\n",
                self.below(5000),
                self.below(8000) as f64 / 10.0 - 400.0,
                self.below(8000) as f64 / 10.0 - 400.0,
                self.below(2000) as f64 / 1000.0,
                self.below(360),
                self.below(2)
            );
        }
        s.truncate(n.saturating_sub(8));
        s += "</data>\n";
        s.into_bytes()
    }

    /// Binary level/record data: fixed-size records with small varying fields.
    pub fn records(&mut self, n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        let mut id = 0u32;
        while v.len() < n {
            id += 1;
            v.extend_from_slice(&id.to_le_bytes());
            v.extend_from_slice(&(self.below(16) as u16).to_le_bytes());
            v.extend_from_slice(&(self.below(9) as u16 * 80).to_le_bytes());
            v.extend_from_slice(&(self.below(5) as u16).to_le_bytes());
            v.extend_from_slice(&[0; 6]);
            v.extend_from_slice(&(self.below(100_000) as u32).to_le_bytes());
        }
        v.truncate(n);
        v
    }
}

// ---- x86-64 executable -------------------------------------------------------------

#[derive(Clone)]
enum Insn {
    Bytes(Vec<u8>),
    /// CALL/JMP rel32 to the start of function `.1`.
    Branch(u8, usize),
    FuncStart(usize),
}

#[derive(Clone)]
struct Exe {
    code: Vec<Insn>,
    funcs: usize,
    data: Vec<u8>,
}

const SNIPPETS: &[&[u8]] = &[
    &[0x48, 0x89, 0x5C, 0x24, 0x08],
    &[0x48, 0x89, 0x74, 0x24, 0x10],
    &[0x48, 0x83, 0xEC, 0x28],
    &[0x48, 0x83, 0xC4, 0x28],
    &[0x48, 0x8B, 0xC1],
    &[0x48, 0x8B, 0xD9],
    &[0x33, 0xC0],
    &[0x85, 0xC0],
    &[0x74, 0x0A],
    &[0x75, 0x12],
    &[0xC3],
    &[0xCC, 0xCC, 0xCC],
    &[0x8B, 0x45, 0xF8],
    &[0x89, 0x45, 0xFC],
    &[0x48, 0x8D, 0x4C, 0x24, 0x20],
    &[0x0F, 0x1F, 0x44, 0x00, 0x00],
    &[0x40, 0x53],
    &[0x5B],
];

impl Exe {
    fn generate(rng: &mut Rng, size: usize) -> Exe {
        let mut code = Vec::new();
        let funcs = (size / 400).max(4);
        let mut bytes = 0;
        let mut f = 0;
        while bytes < size {
            if f < funcs && (code.is_empty() || rng.chance(0.004)) {
                code.push(Insn::FuncStart(f));
                code.push(Insn::Bytes(vec![0x40, 0x53, 0x48, 0x83, 0xEC, 0x20]));
                f += 1;
                bytes += 6;
            }
            let insn = random_insn(rng, funcs);
            bytes += match &insn {
                Insn::Bytes(b) => b.len(),
                _ => 5,
            };
            code.push(insn);
        }
        while f < funcs {
            code.push(Insn::FuncStart(f));
            code.push(Insn::Bytes(vec![0xC3]));
            f += 1;
        }
        let mut data = Vec::new();
        while data.len() < size / 3 {
            data.extend_from_slice(rng.sentence().as_bytes());
            data.push(0);
            if rng.chance(0.3) {
                data.extend_from_slice(&rng.records(64));
            }
        }
        Exe { code, funcs, data }
    }

    fn layout(&self) -> Vec<u8> {
        let mut starts = vec![0usize; self.funcs];
        let mut pos = 0;
        for i in &self.code {
            match i {
                Insn::FuncStart(f) => starts[*f] = pos,
                Insn::Bytes(b) => pos += b.len(),
                Insn::Branch(..) => pos += 5,
            }
        }
        let mut out = pe_header();
        let base = out.len();
        for i in &self.code {
            match i {
                Insn::FuncStart(_) => {}
                Insn::Bytes(b) => out.extend_from_slice(b),
                Insn::Branch(op, f) => {
                    let here = out.len() - base + 5;
                    let rel = starts[*f] as i64 - here as i64;
                    out.push(*op);
                    out.extend_from_slice(&(rel as i32).to_le_bytes());
                }
            }
        }
        out.extend_from_slice(&self.data);
        out
    }
}

fn random_insn(rng: &mut Rng, funcs: usize) -> Insn {
    match rng.below(100) {
        0..=11 => Insn::Branch(0xE8, rng.below(funcs as u64) as usize),
        12..=14 => Insn::Branch(0xE9, rng.below(funcs as u64) as usize),
        15..=59 => Insn::Bytes(SNIPPETS[rng.below(SNIPPETS.len() as u64) as usize].to_vec()),
        60..=79 => {
            // mov reg, [rip+disp32] / lea: rip-relative data references
            let mut v = vec![0x48, 0x8B, 0x05 + 8 * rng.below(8) as u8];
            v.extend_from_slice(&(rng.below(1 << 20) as u32).to_le_bytes());
            Insn::Bytes(v)
        }
        _ => {
            // mov dword [rsp+x], imm32
            let mut v = vec![0xC7, 0x44, 0x24, (rng.below(16) * 8) as u8];
            v.extend_from_slice(&(rng.below(1000) as u32).to_le_bytes());
            Insn::Bytes(v)
        }
    }
}

fn pe_header() -> Vec<u8> {
    let mut h = vec![0u8; 0x400];
    h[0..2].copy_from_slice(b"MZ");
    h[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    h[0x80..0x84].copy_from_slice(b"PE\0\0");
    h[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
    h[0x40..0x80].copy_from_slice(&b"This program cannot be run in DOS mode.\r\r\n$\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0"[..64]);
    h
}

// ---- PopCap-style pak ----------------------------------------------------------------

#[derive(Clone)]
struct PakEntry {
    name: String,
    data: Vec<u8>,
}

fn pak_bytes(entries: &[PakEntry]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&0xBAC04AC0u32.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    for (i, e) in entries.iter().enumerate() {
        v.push(0);
        v.push(e.name.len() as u8);
        v.extend_from_slice(e.name.as_bytes());
        v.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        v.extend_from_slice(&(0x01D0_0000_0000_0000u64 + i as u64 * 7919).to_le_bytes());
    }
    v.push(0x80);
    for e in entries {
        v.extend_from_slice(&e.data);
    }
    v.iter_mut().for_each(|b| *b ^= 0xF7);
    v
}

fn pak_entry(rng: &mut Rng, i: usize, scale: f64) -> PakEntry {
    let sz = |rng: &mut Rng, lo: u64, hi: u64| ((rng.range(lo, hi) as f64 * scale) as usize).max(64);
    match rng.below(10) {
        0..=3 => PakEntry { name: format!("images/{}_{i}.png", rng.word()), data: { let n = sz(rng, 20_000, 400_000); rng.noise(n) } },
        4..=6 => PakEntry { name: format!("reanim/{}_{i}.reanim", rng.word()), data: { let n = sz(rng, 10_000, 300_000); rng.xml(n) } },
        7 => PakEntry { name: format!("particles/{}_{i}.xml", rng.word()), data: { let n = sz(rng, 2_000, 40_000); rng.xml(n) } },
        8 => PakEntry { name: format!("properties/{}_{i}.txt", rng.word()), data: { let n = sz(rng, 1_000, 60_000); rng.text(n) } },
        _ => PakEntry { name: format!("compiled/{}_{i}.bin", rng.word()), data: { let n = sz(rng, 5_000, 200_000); rng.records(n) } },
    }
}

// ---- corpus ---------------------------------------------------------------------------

pub struct Scenario {
    pub name: &'static str,
    pub description: &'static str,
    pub modded: PathBuf,
}

pub struct Corpus {
    pub base: PathBuf,
    pub scenarios: Vec<Scenario>,
}

pub const SCENARIOS: &[(&str, &str)] = &[
    ("exe-codecave", "24 in-place byte patches in the executable"),
    ("exe-relink", "64 KiB of new code inserted mid-executable; every CALL after it shifts"),
    ("pak-repack", "main.pak repacked: 30 inner files edited/replaced, 10 added"),
    ("assets", "300 new small files, 50 edited, 20 deleted, 30 moved, 5 sounds replaced, dir changes"),
    ("full-mod", "all of the above together"),
];

struct Model {
    exe: Exe,
    pak: Vec<PakEntry>,
    small: Vec<String>,
    sounds: Vec<String>,
    xml: Vec<String>,
}

/// Writes `root/base` and one `root/<scenario>` folder per scenario.
/// `scale` 1.0 gives a ~300 MB base; tests use ~0.02.
pub fn generate(root: &Path, scale: f64, seed: u64) -> io::Result<Corpus> {
    let base = root.join("base");
    if base.exists() {
        fs::remove_dir_all(&base)?;
    }
    let mut rng = Rng::new(seed);
    let s = |n: f64| ((n * scale) as usize).max(1);

    let exe = Exe::generate(&mut rng, s(8e6).max(64 << 10));
    put(&base.join("Game.exe"), &exe.layout())?;

    let pak: Vec<PakEntry> = (0..s(700.0).max(40)).map(|i| pak_entry(&mut rng, i, scale.sqrt())).collect();
    let mut pak = pak;
    pak.sort_by(|a, b| a.name.cmp(&b.name));
    put(&base.join("main.pak"), &pak_bytes(&pak))?;

    let mut sounds = Vec::new();
    for i in 0..s(40.0).max(4) {
        let p = format!("sounds/{}_{i}.ogg", rng.word());
        let n = s(rng.range(1_000_000, 3_000_000) as f64);
        put(&base.join(&p), &rng.noise(n))?;
        sounds.push(p);
    }
    let mut xml = Vec::new();
    for i in 0..40 {
        let p = format!("properties/{}_{i}.xml", rng.word());
        put(&base.join(&p), &{ let n = rng.range(2_000, 50_000) as usize; rng.xml(n) })?;
        xml.push(p);
    }
    for i in 0..s(30.0).max(3) {
        put(&base.join(format!("levels/level{i:03}.dat")), &rng.records(s(500_000.0)))?;
    }
    let mut small = Vec::new();
    for i in 0..s(2000.0).max(60) {
        let dir = ["data/ui", "data/fonts", "data/strings", "data/compiled", "data/legacy"][i % 5];
        let (ext, data) = match rng.below(4) {
            0 => ("txt", { let n = rng.range(1_000, 20_000) as usize; rng.text(n) }),
            1 => ("xml", { let n = rng.range(1_000, 20_000) as usize; rng.xml(n) }),
            2 => ("dat", { let n = rng.range(1_000, 20_000) as usize; rng.records(n) }),
            _ => ("img", { let n = rng.range(1_000, 20_000) as usize; rng.noise(n) }),
        };
        let p = format!("{dir}/{}_{i}.{ext}", rng.word());
        put(&base.join(&p), &data)?;
        small.push(p);
    }
    fs::create_dir_all(base.join("userdata"))?;
    put(&base.join("readme.txt"), &rng.text(4000))?;

    let model = Model { exe, pak, small, sounds, xml };
    let mut scenarios = Vec::new();
    for &(name, description) in SCENARIOS {
        let dir = root.join(name);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        link_tree(&base, &dir)?;
        let mut rng = Rng::new(seed ^ fxhash(name));
        match name {
            "exe-codecave" => exe_codecave(&dir, &model, &mut rng)?,
            "exe-relink" => exe_relink(&dir, &model, &mut rng)?,
            "pak-repack" => pak_repack(&dir, &model, &mut rng, scale)?,
            "assets" => assets(&dir, &model, &mut rng, scale)?,
            "full-mod" => {
                exe_relink(&dir, &model, &mut rng)?;
                pak_repack(&dir, &model, &mut rng, scale)?;
                assets(&dir, &model, &mut rng, scale)?;
            }
            _ => unreachable!(),
        }
        scenarios.push(Scenario { name, description, modded: dir });
    }
    Ok(Corpus { base, scenarios })
}

fn fxhash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn exe_codecave(dir: &Path, m: &Model, rng: &mut Rng) -> io::Result<()> {
    let mut bytes = m.exe.layout();
    let code_len = bytes.len() - m.exe.data.len();
    for _ in 0..24 {
        let at = rng.range(0x400, (code_len - 16) as u64) as usize;
        let n = rng.range(2, 12) as usize;
        for b in &mut bytes[at..at + n] {
            *b = if rng.chance(0.5) { 0x90 } else { rng.next_u64() as u8 };
        }
    }
    put(&dir.join("Game.exe"), &bytes)
}

fn exe_relink(dir: &Path, m: &Model, rng: &mut Rng) -> io::Result<()> {
    let mut exe = m.exe.clone();
    let at = exe.code.len() / 3 + rng.below(exe.code.len() as u64 / 3) as usize;
    let new_funcs = 40;
    let mut inserted = Vec::new();
    let mut bytes = 0;
    let mut f = 0;
    while bytes < 64 << 10 {
        if f < new_funcs && rng.chance(0.01) {
            inserted.push(Insn::FuncStart(exe.funcs + f));
            f += 1;
        }
        let i = random_insn(rng, exe.funcs + f.max(1));
        bytes += match &i {
            Insn::Bytes(b) => b.len(),
            _ => 5,
        };
        inserted.push(i);
    }
    for k in f..new_funcs {
        inserted.push(Insn::FuncStart(exe.funcs + k));
    }
    inserted.push(Insn::Bytes(vec![0xC3]));
    exe.funcs += new_funcs;
    exe.code.splice(at..at, inserted);
    // A few existing call sites now go through the new code.
    for _ in 0..50 {
        let i = rng.below(exe.code.len() as u64) as usize;
        if let Insn::Branch(_, t) = &mut exe.code[i] {
            *t = exe.funcs - 1 - rng.below(new_funcs as u64) as usize;
        }
    }
    exe.data.extend_from_slice(b"Modded build\0");
    put(&dir.join("Game.exe"), &exe.layout())
}

fn pak_repack(dir: &Path, m: &Model, rng: &mut Rng, scale: f64) -> io::Result<()> {
    let mut pak = m.pak.clone();
    for _ in 0..30 {
        let i = rng.below(pak.len() as u64) as usize;
        let e = &mut pak[i];
        if e.name.ends_with(".png") {
            e.data = { let n = e.data.len() * 9 / 10 + rng.below(4096) as usize; rng.noise(n) };
        } else {
            // Edit a few spots, grow a little.
            for _ in 0..rng.range(1, 6) {
                let at = rng.below(e.data.len() as u64) as usize;
                let ins = { let n = rng.range(10, 400) as usize; rng.text(n) };
                e.data.splice(at..at, ins);
            }
        }
    }
    for i in 0..10 {
        pak.push(pak_entry(rng, 100_000 + i, scale.sqrt()));
    }
    pak.sort_by(|a, b| a.name.cmp(&b.name));
    put(&dir.join("main.pak"), &pak_bytes(&pak))
}

fn assets(dir: &Path, m: &Model, rng: &mut Rng, scale: f64) -> io::Result<()> {
    let s = |n: f64| ((n * scale) as usize).max(3);
    for i in 0..s(300.0) {
        let (p, data) = match rng.below(4) {
            0 => (format!("mods/strings/{}_{i}.txt", rng.word()), { let n = rng.range(500, 12_000) as usize; rng.text(n) }),
            1 => (format!("mods/reanim/{}_{i}.xml", rng.word()), { let n = rng.range(1_000, 30_000) as usize; rng.xml(n) }),
            2 => (format!("mods/levels/{}_{i}.dat", rng.word()), { let n = rng.range(1_000, 30_000) as usize; rng.records(n) }),
            _ => (format!("mods/images/{}_{i}.img", rng.word()), { let n = rng.range(2_000, 60_000) as usize; rng.noise(n) }),
        };
        put(&dir.join(p), &data)?;
    }
    let mut small = m.small.clone();
    for _ in 0..s(50.0) {
        let p = &small[rng.below(small.len() as u64) as usize];
        let mut d = fs::read(dir.join(p))?;
        let at = rng.below(d.len() as u64) as usize;
        let end = (at + rng.range(1, 200) as usize).min(d.len());
        d.splice(at..end, { let n = rng.range(1, 300) as usize; rng.text(n) });
        put(&dir.join(p), &d)?;
    }
    for x in m.xml.iter().take(5) {
        let mut d = fs::read(dir.join(x))?;
        d.extend_from_slice(&rng.xml(2000));
        put(&dir.join(x), &d)?;
    }
    // Delete 20, move 30 into a renamed folder.
    for _ in 0..s(20.0) {
        let i = rng.below(small.len() as u64) as usize;
        let p = small.swap_remove(i);
        let _ = fs::remove_file(dir.join(&p));
    }
    for _ in 0..s(30.0) {
        let i = rng.below(small.len() as u64) as usize;
        let p = small.swap_remove(i);
        let to = dir.join(p.replacen("data/", "data/moved/", 1));
        fs::create_dir_all(to.parent().unwrap())?;
        let _ = fs::rename(dir.join(&p), to);
    }
    // Move and edit (the rename detector should diff these against their old paths).
    for _ in 0..s(5.0) {
        let i = rng.below(small.len() as u64) as usize;
        let p = small.swap_remove(i);
        let Ok(mut d) = fs::read(dir.join(&p)) else { continue };
        let at = rng.below(d.len() as u64) as usize;
        d.splice(at..at, *b"edited after the move");
        let _ = fs::remove_file(dir.join(&p));
        put(&dir.join(p.replacen("data/", "data/renamed/", 1)), &d)?;
    }
    for snd in m.sounds.iter().take(5) {
        let len = fs::metadata(dir.join(snd))?.len() as usize;
        put(&dir.join(snd), &rng.noise(len))?;
    }
    // Remove a whole folder, add an empty one.
    let _ = fs::remove_dir_all(dir.join("data/legacy"));
    fs::create_dir_all(dir.join("mods/empty"))?;
    Ok(())
}

/// Two multi-GB files for the large-file path: `base/big.bin` and `modded/big.bin`.
pub fn generate_large(root: &Path, size: u64, seed: u64) -> io::Result<(PathBuf, PathBuf)> {
    use std::io::Write;
    let base = root.join("large-base");
    let modded = root.join("large-modded");
    fs::create_dir_all(&base)?;
    fs::create_dir_all(&modded)?;
    let mut rng = Rng::new(seed);
    // Blocks of mixed content; the modded file inserts, rewrites and appends blocks.
    let block = 1 << 20;
    let n = (size / block as u64) as usize;
    let mut fb = io::BufWriter::new(fs::File::create(base.join("big.bin"))?);
    let mut fm = io::BufWriter::new(fs::File::create(modded.join("big.bin"))?);
    for i in 0..n {
        let mut r = Rng::new(seed ^ i as u64);
        let data = match i % 4 {
            0 => r.noise(block),
            1 => r.records(block),
            2 => r.xml(block),
            _ => r.text(block),
        };
        fb.write_all(&data)?;
        if i == n / 3 {
            fm.write_all(&rng.noise(block))?; // inserted block shifts everything after it
        }
        if i % 97 == 5 {
            let mut d = data.clone();
            for _ in 0..8 {
                let at = rng.below((block - 4096) as u64) as usize;
                d[at..at + 4096].copy_from_slice(&rng.noise(4096));
            }
            fm.write_all(&d)?;
        } else {
            fm.write_all(&data)?;
        }
    }
    fm.write_all(&rng.noise(10 << 20))?;
    fb.flush()?;
    fm.flush()?;
    Ok((base, modded))
}

// ---- file helpers ----------------------------------------------------------------------

/// Writes a file, replacing (not overwriting in place) any existing one so hard-linked
/// copies of the base stay untouched.
pub fn put(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let _ = fs::remove_file(path);
    fs::write(path, data)
}

/// Mirrors `src` into `dst` using hard links (copies if linking is unsupported).
pub fn link_tree(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            link_tree(&e.path(), &to)?;
        } else if fs::hard_link(e.path(), &to).is_err() {
            fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

/// Real copy (used for game folders that get patched).
pub fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

/// Sorted (relative path, contents hash) listing plus directories, for comparing trees.
pub fn tree_listing(root: &Path) -> io::Result<Vec<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if rel.starts_with(".patcher-") {
                continue;
            }
            if e.file_type()?.is_dir() {
                out.push(format!("{rel}/"));
                walk(root, &e.path(), out)?;
            } else {
                let d = fs::read(e.path())?;
                let h = d.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
                out.push(format!("{rel} {} {h:016x}", d.len()));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}
