use serde_json::Value;
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
#[derive(Default)]
struct Cursor {
    inode: u64,
    offset: u64,
    rest: Vec<u8>,
    skip: bool,
}
#[derive(Default)]
pub struct Tail {
    state: HashMap<PathBuf, Cursor>,
}
impl Tail {
    pub fn forget_under(&mut self, root: &Path) {
        self.state.retain(|p, _| !p.starts_with(root));
    }
    pub fn pump(&mut self, path: &Path, backfill: u64) -> std::io::Result<(bool, Vec<Value>)> {
        let m = std::fs::metadata(path)?;
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            m.ino()
        };
        #[cfg(not(unix))]
        let inode = 0;
        let reset = self
            .state
            .get(path)
            .is_some_and(|s| s.inode != inode || m.len() < s.offset);
        if reset {
            self.state.remove(path);
        }
        let c = self.state.entry(path.into()).or_insert_with(|| {
            let offset = if reset {
                0
            } else {
                m.len().saturating_sub(backfill)
            };
            Cursor {
                inode,
                offset,
                skip: offset > 0,
                ..Cursor::default()
            }
        });
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(c.offset))?;
        let mut out = Vec::new();
        let mut buf = [0; 262144];
        while c.offset < m.len() {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            c.offset += n as u64;
            for b in &buf[..n] {
                if *b == b'\n' {
                    if !c.skip {
                        if let Ok(v) = serde_json::from_slice(&c.rest) {
                            out.push(v);
                        }
                    }
                    c.rest.clear();
                    c.skip = false;
                } else if !c.skip {
                    c.rest.push(*b);
                    if c.rest.len() > 2 * 1024 * 1024 {
                        c.rest.clear();
                        c.skip = true;
                    }
                }
            }
        }
        Ok((reset, out))
    }
}
pub fn walk(dir: &Path, ending: &str, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            let Ok(ty) = e.file_type() else { continue };
            if ty.is_dir()
                && depth < 6
                && e.file_name() != "node_modules"
                && e.file_name() != ".git"
            {
                out.extend(walk(&p, ending, depth + 1));
            } else if ty.is_file()
                && p.file_name()
                    .is_some_and(|s| s.to_string_lossy().ends_with(ending))
            {
                out.push(p);
            }
        }
    }
    out
}
