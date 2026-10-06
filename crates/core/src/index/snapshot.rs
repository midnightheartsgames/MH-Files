//! Снимок индекса на диске: двоичный, версионный, с контрольной суммой. Загружается за
//! доли секунды, поэтому поиск по дискам работает сразу после запуска — пока свежий обход
//! идёт в фоне.
//!
//! Удалённые узлы в снимок не попадают: при записи индекс заодно ужимается.

use std::path::PathBuf;

use super::{DELETED, JournalMark, NONE, NodeInfo, ROOT, VolumeIndex};

const MAGIC: &[u8; 8] = b"MHINDEX\n";
const VERSION: u32 = 1;

pub fn write(index: &VolumeIndex) -> Vec<u8> {
    let count = index.parent.len();
    // Новые номера: удалённые выпадают, порядок (родитель раньше ребёнка) сохраняется.
    let mut remap = vec![NONE; count];
    let mut alive = 0u32;
    for (node, slot) in remap.iter_mut().enumerate() {
        if index.flags[node] & DELETED == 0 {
            *slot = alive;
            alive += 1;
        }
    }
    let mut out = Vec::with_capacity(count * 40 + 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let root = index.root.to_string_lossy();
    out.extend_from_slice(&(root.len() as u32).to_le_bytes());
    out.extend_from_slice(root.as_bytes());
    out.extend_from_slice(&index.built_at.to_le_bytes());
    match index.journal {
        Some(mark) => {
            out.push(1);
            out.extend_from_slice(&mark.id.to_le_bytes());
            out.extend_from_slice(&mark.next_usn.to_le_bytes());
        }
        None => {
            out.push(0);
            out.extend_from_slice(&[0; 16]);
        }
    }
    out.extend_from_slice(&alive.to_le_bytes());
    for node in 0..count {
        if remap[node] == NONE {
            continue;
        }
        let parent = index.parent[node];
        let parent = if parent == NONE { NONE } else { remap[parent as usize] };
        out.extend_from_slice(&parent.to_le_bytes());
        out.push(index.flags[node]);
        out.extend_from_slice(&index.size[node].to_le_bytes());
        out.extend_from_slice(&index.modified[node].to_le_bytes());
        let name = index.name(node as u32).as_bytes();
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
    }
    let checksum = fnv(&out);
    out.extend_from_slice(&checksum.to_le_bytes());
    out
}

pub fn read(bytes: &[u8]) -> Result<VolumeIndex, String> {
    let broken = || "снимок индекса повреждён".to_string();
    if bytes.len() < MAGIC.len() + 8 || &bytes[..MAGIC.len()] != MAGIC {
        return Err("это не снимок индекса MH Files".into());
    }
    let (payload, tail) = bytes.split_at(bytes.len() - 8);
    if fnv(payload) != u64::from_le_bytes(tail.try_into().map_err(|_| broken())?) {
        return Err(broken());
    }
    let mut reader = Reader { bytes: payload, at: MAGIC.len() };
    let version = reader.u32().ok_or_else(broken)?;
    if version != VERSION {
        return Err(format!("снимок индекса версии {version} — будет построен заново"));
    }
    let root_len = reader.u32().ok_or_else(broken)? as usize;
    let root = String::from_utf8(reader.take(root_len).ok_or_else(broken)?.to_vec())
        .map_err(|_| broken())?;
    let built_at = reader.i64().ok_or_else(broken)?;
    let has_journal = reader.take(1).ok_or_else(broken)?[0] == 1;
    let journal_id = reader.u64().ok_or_else(broken)?;
    let next_usn = reader.i64().ok_or_else(broken)?;
    let count = reader.u32().ok_or_else(broken)? as usize;
    let mut index = VolumeIndex::new(PathBuf::from(root));
    index.built_at = built_at;
    index.journal = has_journal.then_some(JournalMark { id: journal_id, next_usn });
    for node in 0..count {
        let parent = reader.u32().ok_or_else(broken)?;
        let flags = reader.take(1).ok_or_else(broken)?[0];
        let size = reader.u64().ok_or_else(broken)?;
        let modified = reader.i64().ok_or_else(broken)?;
        let len = reader.u16().ok_or_else(broken)? as usize;
        let name =
            std::str::from_utf8(reader.take(len).ok_or_else(broken)?).map_err(|_| broken())?;
        if node == 0 {
            // Корень уже создан конструктором.
            continue;
        }
        if parent as usize >= node {
            return Err(broken());
        }
        let info = NodeInfo {
            is_dir: flags & super::DIR != 0,
            size,
            modified,
            hidden: flags & super::HIDDEN != 0,
            system: flags & super::SYSTEM != 0,
        };
        index.add(parent, name, info);
    }
    if reader.at != payload.len() || index.parent.len() != count.max(1) {
        return Err(broken());
    }
    let _ = ROOT;
    Ok(index)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(slice)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// FNV-1a: ловит битый или недописанный файл, а не злоумышленника.
fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::index::tests::sample;

    #[test]
    fn round_trip_drops_deleted_nodes() {
        let mut index = sample();
        index.built_at = 42;
        index.journal = Some(JournalMark { id: 7, next_usn: 99 });
        index.remove(index.lookup(Path::new("/d/Photos")).unwrap());
        let bytes = write(&index);
        let loaded = read(&bytes).unwrap();
        assert_eq!(loaded.len(), index.len());
        assert_eq!(loaded.garbage(), 0.0);
        assert_eq!(loaded.built_at, 42);
        assert_eq!(loaded.journal, Some(JournalMark { id: 7, next_usn: 99 }));
        let node = loaded.lookup(Path::new("/d/Models/llama.gguf")).unwrap();
        assert_eq!(loaded.size[node as usize], 7 << 30);
        assert!(loaded.lookup(Path::new("/d/Photos")).is_none());
        assert_eq!(loaded.root(), Path::new("/d"));
    }

    #[test]
    fn damage_is_detected() {
        let bytes = write(&sample());
        let mut broken = bytes.clone();
        let middle = broken.len() / 2;
        broken[middle] ^= 0xFF;
        assert!(read(&broken).is_err());
        assert!(read(&bytes[..bytes.len() - 3]).is_err());
        assert!(read(b"garbage").is_err());
    }
}
