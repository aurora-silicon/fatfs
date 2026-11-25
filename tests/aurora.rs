//! Compatibility and read correctness for the m1n1 dependency changes.

use std::cell::RefCell;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use fatfs::{FileSystem, FsOptions, StdIoWrapper};

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

// Synthetic, explicitly formatted FAT32 with fewer than 65,525 data clusters.
// This is a compatibility fixture, not evidence about any physical ESP.
fn small_fat32() -> Vec<u8> {
    let mut image = vec![0; 2048 * 512];
    image[..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    image[3..11].copy_from_slice(b"MSWIN4.1");
    put16(&mut image, 11, 512);
    image[13] = 1;
    put16(&mut image, 14, 32);
    image[16] = 1;
    image[21] = 0xf8;
    put32(&mut image, 32, 2048);
    put32(&mut image, 36, 16);
    put32(&mut image, 44, 2);
    put16(&mut image, 48, 1);
    image[64] = 0x80;
    image[66] = 0x29;
    image[71..82].copy_from_slice(b"NO NAME    ");
    image[82..90].copy_from_slice(b"FAT32   ");
    image[510..512].copy_from_slice(&[0x55, 0xaa]);

    put32(&mut image, 512, 0x4161_5252);
    put32(&mut image, 512 + 484, 0x6141_7272);
    put32(&mut image, 512 + 488, u32::MAX);
    put32(&mut image, 512 + 492, u32::MAX);
    put32(&mut image, 512 + 508, 0xaa55_0000);

    let fat = 32 * 512;
    for (cluster, value) in [
        (0, 0x0fff_fff8),
        (1, 0x0fff_ffff),
        (2, 0x0fff_ffff),
        (3, 4),
        (4, 0x0fff_ffff),
    ] {
        put32(&mut image, fat + cluster * 4, value);
    }
    let root = 48 * 512;
    image[root..root + 11].copy_from_slice(b"STAGE2  BIN");
    image[root + 11] = 0x20;
    put16(&mut image, root + 26, 3);
    put32(&mut image, root + 28, 700);
    for (i, byte) in image[root + 512..root + 512 + 700].iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    image
}

#[test]
fn small_fat32_requires_explicit_compatibility_feature() {
    let result = FileSystem::new(Cursor::new(small_fat32()), FsOptions::new());
    #[cfg(not(feature = "explicit-fat32"))]
    assert!(matches!(result, Err(fatfs::Error::CorruptedFileSystem)));
    #[cfg(feature = "explicit-fat32")]
    {
        let fs = result.unwrap();
        assert_eq!(fs.fat_type(), fatfs::FatType::Fat32);
        let mut file = fs.root_dir().open_file("stage2.bin").unwrap();
        let mut content = Vec::new();
        file.read_to_end(&mut content).unwrap();
        assert_eq!(content, (0..700).map(|i| (i % 251) as u8).collect::<Vec<_>>());
    }
}

#[cfg(feature = "lfn")]
#[test]
fn entry_name_matching_accepts_long_names_and_short_aliases() {
    let image = include_bytes!("../resources/fat16.img").to_vec();
    let fs = FileSystem::new(Cursor::new(image), FsOptions::new()).unwrap();
    let dir = fs.root_dir().open_dir("very-long-dir-name").unwrap();
    let entry = dir.iter().map(Result::unwrap).find(|e| e.is_file()).unwrap();
    assert!(entry.eq_name("VERY-LONG-FILE-NAME.TXT"));
    assert!(entry.eq_name("very-l~1.txt"));
    assert!(!entry.eq_name("another-file.txt"));
}

fn file_image(content: &[u8], fragmented: bool) -> Vec<u8> {
    let mut image = vec![0; 8 * 1024 * 1024];
    {
        let mut disk = StdIoWrapper::new(Cursor::new(&mut image));
        fatfs::format_volume(&mut disk, fatfs::FormatVolumeOptions::new().bytes_per_cluster(512)).unwrap();
        let fs = FileSystem::new(disk, FsOptions::new()).unwrap();
        let root = fs.root_dir();
        let mut file = root.create_file("payload.bin").unwrap();
        let mut filler = root.create_file("filler.bin").unwrap();
        for chunk in content.chunks(512) {
            file.write_all(chunk).unwrap();
            if fragmented {
                filler.write_all(&[0x55; 512]).unwrap();
            }
        }
    }
    image
}

struct MeasuredStorage {
    disk: Cursor<Vec<u8>>,
    reads: Rc<RefCell<Vec<usize>>>,
    max_read: usize,
}

impl Read for MeasuredStorage {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let len = bytes.len().min(self.max_read);
        self.reads.borrow_mut().push(len);
        self.disk.read(&mut bytes[..len])
    }
}

impl Write for MeasuredStorage {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.disk.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for MeasuredStorage {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.disk.seek(position)
    }
}

#[test]
fn contiguous_file_is_read_in_one_data_transfer() {
    let content: Vec<u8> = (0..65536).map(|i| (i % 251) as u8).collect();
    let reads = Rc::new(RefCell::new(Vec::new()));
    let storage = MeasuredStorage {
        disk: Cursor::new(file_image(&content, false)),
        reads: Rc::clone(&reads),
        max_read: usize::MAX,
    };
    let fs = FileSystem::new(storage, FsOptions::new()).unwrap();
    let mut file = fs.root_dir().open_file("payload.bin").unwrap();
    reads.borrow_mut().clear();
    let mut actual = vec![0; content.len()];
    assert_eq!(file.read(&mut actual).unwrap(), content.len());
    assert_eq!(actual, content);
    assert_eq!(reads.borrow().iter().filter(|&&len| len > 512).count(), 1);
    assert_eq!(file.read(&mut [0; 1]).unwrap(), 0);
}

#[test]
fn short_reads_and_fragmented_chains_preserve_data_and_seek_position() {
    let content: Vec<u8> = (0..32771).map(|i| (i % 251) as u8).collect();
    for fragmented in [false, true] {
        let storage = MeasuredStorage {
            disk: Cursor::new(file_image(&content, fragmented)),
            reads: Rc::new(RefCell::new(Vec::new())),
            max_read: 137,
        };
        let fs = FileSystem::new(storage, FsOptions::new()).unwrap();
        let mut file = fs.root_dir().open_file("payload.bin").unwrap();
        let mut actual = vec![0; content.len()];
        file.read_exact(&mut actual).unwrap();
        assert_eq!(actual, content);
        assert_eq!(file.read(&mut [0; 1]).unwrap(), 0);
        for start in [0, 511, 512, 513, 1023, 32700] {
            file.seek(SeekFrom::Start(start)).unwrap();
            let mut tail = Vec::new();
            file.read_to_end(&mut tail).unwrap();
            assert_eq!(tail, content[start as usize..]);
        }
    }
}

// A FAT16 volume with payload.bin (3 clusters) and sub/inner.txt; returns the image, the byte
// offsets of the two directory entries and of the FAT, and payload.bin's first cluster.
fn corruptible_image() -> (Vec<u8>, usize, usize, usize, u16) {
    let mut image = file_image(&[0xa5; 1536], false);
    {
        let disk = StdIoWrapper::new(Cursor::new(&mut image));
        let fs = FileSystem::new(disk, FsOptions::new()).unwrap();
        let sub = fs.root_dir().create_dir("sub").unwrap();
        sub.create_file("inner.txt").unwrap().write_all(b"inner").unwrap();
    }
    let find = |name: &[u8; 11]| image.windows(11).position(|w| w == name).unwrap();
    let payload = find(b"PAYLOAD BIN");
    let sub = find(b"SUB        ");
    let fat = usize::from(u16::from_le_bytes([image[14], image[15]])) * 512;
    let first = u16::from_le_bytes([image[payload + 26], image[payload + 27]]);
    (image, payload, sub, fat, first)
}

// Cluster numbers from directory entries and the FAT that name no data cluster (0 and 1 are
// reserved, the rest past the volume's last cluster) make reads fail; nothing panics.
#[test]
fn invalid_cluster_numbers_are_corruption() {
    let (_, _, _, _, first) = corruptible_image();
    let past_end = 0xfff0; // below the FAT16 bad-cluster and end-of-chain markers
    let cases: Vec<(&str, Box<dyn Fn(&mut Vec<u8>, usize, usize, usize)>)> = vec![
        ("file starts at cluster 1", Box::new(|im, p, _, _| put16(im, p + 26, 1))),
        (
            "file starts past the volume",
            Box::new(move |im, p, _, _| put16(im, p + 26, past_end)),
        ),
        (
            "FAT links to cluster 1",
            Box::new(move |im, _, _, fat| put16(im, fat + 2 * usize::from(first), 1)),
        ),
        (
            "FAT links past the volume",
            Box::new(move |im, _, _, fat| put16(im, fat + 2 * usize::from(first), past_end)),
        ),
    ];
    for (what, corrupt) in &cases {
        let (mut image, payload, sub, fat, _) = corruptible_image();
        corrupt(&mut image, payload, sub, fat);
        let fs = FileSystem::new(Cursor::new(image), FsOptions::new()).unwrap();
        let mut file = fs.root_dir().open_file("payload.bin").unwrap();
        let mut content = Vec::new();
        let err = file.read_to_end(&mut content).expect_err(what);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{what}");
        let mut file = fs.root_dir().open_file("payload.bin").unwrap();
        assert!(file.extents().any(|e| e.is_err()), "{what}: extents");
        let mut file = fs.root_dir().open_file("payload.bin").unwrap();
        assert!(
            file.seek(SeekFrom::Start(1000)).is_err() || file.read(&mut [0; 1]).is_err(),
            "{what}: seek"
        );
    }
    for cluster in [1, past_end] {
        let (mut image, _, sub, _, _) = corruptible_image();
        put16(&mut image, sub + 26, cluster);
        let fs = FileSystem::new(Cursor::new(image), FsOptions::new()).unwrap();
        let dir = fs.root_dir().open_dir("sub").unwrap();
        assert!(dir.iter().any(|e| e.is_err()), "directory at cluster {cluster}");
    }
}

// Removing or truncating a file whose chain names a reserved or nonexistent cluster fails as
// corruption: it ends, and it writes no FAT entry outside the volume's data clusters.
#[test]
fn invalid_cluster_chains_are_not_freed_or_truncated() {
    let (_, _, _, _, first) = corruptible_image();
    let past_end = 0xfff0;
    let cases: Vec<(&str, Box<dyn Fn(&mut Vec<u8>, usize, usize)>)> = vec![
        ("file starts at cluster 1", Box::new(|im, p, _| put16(im, p + 26, 1))),
        (
            "file starts past the volume",
            Box::new(move |im, p, _| put16(im, p + 26, past_end)),
        ),
        (
            "FAT links to cluster 1",
            Box::new(move |im, _, fat| put16(im, fat + 2 * usize::from(first), 1)),
        ),
        (
            "FAT links past the volume",
            Box::new(move |im, _, fat| put16(im, fat + 2 * usize::from(first), past_end)),
        ),
    ];
    for remove in [true, false] {
        for (what, corrupt) in &cases {
            let (mut image, payload, _, fat, _) = corruptible_image();
            corrupt(&mut image, payload, fat);
            let reserved = image[fat..fat + 4].to_vec();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut cur = Cursor::new(image);
                let result = {
                    let fs = FileSystem::new(&mut cur, FsOptions::new()).unwrap();
                    if remove {
                        fs.root_dir().remove("payload.bin").map(|_| ())
                    } else {
                        let mut file = fs.root_dir().open_file("payload.bin").unwrap();
                        file.seek(SeekFrom::Start(0)).unwrap();
                        file.truncate().map(|_| ())
                    }
                };
                let image = cur.into_inner();
                tx.send((result.is_err(), image[fat..fat + 4].to_vec())).unwrap();
            });
            let op = if remove { "remove" } else { "truncate" };
            let (failed, after) = rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("{what}: {op} did not finish"));
            assert!(failed, "{what}: {op} succeeded");
            assert_eq!(after, reserved, "{what}: {op} wrote a reserved FAT entry");
        }
    }
}
