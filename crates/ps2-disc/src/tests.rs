//! Tests on synthetic images built in memory; no game data needed.

use super::*;

/// Builds a minimal ISO 9660 image: system area, primary volume descriptor,
/// terminator, a root directory at sector 18 and the files from sector 19 on,
/// followed by `trailing` sectors outside the file system.
fn make_iso(files: &[(&str, &[u8])], trailing: u32) -> Vec<u8> {
    const ROOT_LBA: u32 = 18;
    let mut file_lbas = Vec::new();
    let mut next = ROOT_LBA + 1;
    for (_, data) in files {
        file_lbas.push(next);
        next += u32::try_from(data.len().div_ceil(SECTOR_SIZE))
            .unwrap()
            .max(1);
    }
    let total = next + trailing;
    let mut img = vec![0; total as usize * SECTOR_SIZE];

    let record = |lba: u32, size: u32, is_dir: bool, name: &[u8]| -> Vec<u8> {
        let mut r = vec![0; 33 + name.len() + (name.len() + 1) % 2];
        r[0] = u8::try_from(r.len()).unwrap();
        r[2..6].copy_from_slice(&lba.to_le_bytes());
        r[6..10].copy_from_slice(&lba.to_be_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[14..18].copy_from_slice(&size.to_be_bytes());
        r[25] = if is_dir { 2 } else { 0 };
        r[32] = u8::try_from(name.len()).unwrap();
        r[33..33 + name.len()].copy_from_slice(name);
        r
    };

    let pvd = &mut img[16 * SECTOR_SIZE..17 * SECTOR_SIZE];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[8..40].copy_from_slice(&[b' '; 32]);
    pvd[8..19].copy_from_slice(b"PLAYSTATION");
    pvd[40..72].copy_from_slice(&[b' '; 32]);
    pvd[40..43].copy_from_slice(b"SLY");
    pvd[80..84].copy_from_slice(&total.to_le_bytes());
    pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
    let root = record(ROOT_LBA, 2048, true, &[0]);
    pvd[156..156 + root.len()].copy_from_slice(&root);

    let term = &mut img[17 * SECTOR_SIZE..18 * SECTOR_SIZE];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");

    let mut dir = Vec::new();
    dir.extend(record(ROOT_LBA, 2048, true, &[0]));
    dir.extend(record(ROOT_LBA, 2048, true, &[1]));
    for ((name, data), lba) in files.iter().zip(&file_lbas) {
        let versioned = format!("{name};1");
        dir.extend(record(
            *lba,
            u32::try_from(data.len()).unwrap(),
            false,
            versioned.as_bytes(),
        ));
        let at = *lba as usize * SECTOR_SIZE;
        img[at..at + data.len()].copy_from_slice(data);
    }
    let at = ROOT_LBA as usize * SECTOR_SIZE;
    img[at..at + dir.len()].copy_from_slice(&dir);
    img
}

fn sample() -> Vec<u8> {
    make_iso(
        &[
            (
                "SYSTEM.CNF",
                b"BOOT2 = cdrom0:\\SCUS_974.64;1\r\nVER = 1.00\r\nVMODE = NTSC\r\n",
            ),
            ("SCUS_974.64", &[0x7f, b'E', b'L', b'F', 1, 1, 1]),
            ("BIG.DAT", &[0xAB; 5000]),
        ],
        3,
    )
}

#[test]
fn volume_and_root_directory() {
    let disc = Disc::from_source(sample()).unwrap();
    let volume = disc.volume().unwrap();
    assert_eq!(volume.system_id, "PLAYSTATION");
    assert_eq!(volume.volume_id, "SLY");
    assert_eq!(volume.sectors, disc.sector_count());
    assert_eq!((volume.root.lba, volume.root.is_dir), (18, true));

    let root = disc.root().unwrap();
    let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["SYSTEM.CNF", "SCUS_974.64", "BIG.DAT"]);
    let big = &root[2];
    assert_eq!(
        (big.lba, big.size, big.sector_count(), big.end_lba()),
        (21, 5000, 3, 24)
    );
}

#[test]
fn find_and_read_files() {
    let disc = Disc::from_source(sample()).unwrap();
    let elf = disc.find("scus_974.64;1").unwrap().unwrap();
    assert_eq!(
        disc.read_file(&elf).unwrap(),
        [0x7f, b'E', b'L', b'F', 1, 1, 1]
    );
    assert!(disc.find("MISSING.BIN").unwrap().is_none());
    let big = disc.find("BIG.DAT").unwrap().unwrap();
    assert!(disc.read_file(&big).unwrap().iter().all(|&b| b == 0xAB));
}

#[test]
fn system_cnf_with_and_without_spaces() {
    let disc = Disc::from_source(sample()).unwrap();
    let cnf = disc.system_cnf().unwrap();
    assert_eq!(cnf.boot_file(), "SCUS_974.64");
    assert_eq!(cnf.ver.as_deref(), Some("1.00"));
    assert_eq!(cnf.vmode.as_deref(), Some("NTSC"));

    let tight = SystemCnf::parse("BOOT2=cdrom0:\\SCUS_971.98;1\nVER=1.00\nVMODE=NTSC\n").unwrap();
    assert_eq!(tight.boot_file(), "SCUS_971.98");
    assert!(matches!(
        SystemCnf::parse("VER=1.00\n"),
        Err(DiscError::BadSystemCnf(_))
    ));
}

#[test]
fn raw_sector_reads_and_bounds() {
    let img = sample();
    let disc = Disc::from_source(img.as_slice()).unwrap();
    let mut two = vec![0; 2 * SECTOR_SIZE];
    disc.read_sectors(16, &mut two).unwrap();
    assert_eq!(&two[1..6], b"CD001");
    assert_eq!(&two[SECTOR_SIZE + 1..SECTOR_SIZE + 6], b"CD001");

    let last = disc.sector_count() - 1;
    assert!(disc.sectors(last, 1).is_ok());
    assert!(matches!(
        disc.sectors(last, 2),
        Err(DiscError::OutOfBounds { .. })
    ));
    assert!(matches!(
        disc.read_sectors(0, &mut [0; 100]),
        Err(DiscError::UnalignedBuffer { len: 100 })
    ));
    assert!(matches!(
        disc.read_at(u64::MAX, &mut [0; 1]),
        Err(DiscError::OutOfBounds { .. })
    ));
}

#[test]
fn not_iso9660() {
    let disc = Disc::from_source(vec![0; 20 * SECTOR_SIZE]).unwrap();
    assert!(matches!(disc.volume(), Err(DiscError::NotIso9660(_))));
}

#[test]
fn sha1_known_vectors() {
    assert_eq!(
        Sha1Digest::of(b"abc"),
        Sha1Digest::from_hex("a9993e364706816aba3e25717850c26c9cd0d89d")
    );
    assert_eq!(
        Sha1Digest::of(b"").to_string(),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
}

#[test]
fn whole_image_sha1_matches_one_shot() {
    let img = sample();
    let disc = Disc::from_source(img.as_slice()).unwrap();
    let mut last = 0;
    let streamed = disc.sha1(|done| last = done).unwrap();
    assert_eq!(streamed, Sha1Digest::of(&img));
    assert_eq!(last, img.len() as u64);
}
