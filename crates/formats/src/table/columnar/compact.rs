//! Bounded, iterative preflight of the compact Thrift headers consumed by Parquet.
use std::io::Read;

use super::Limits;

struct Compact<R> {
    reader: R,
    left: usize,
    used: usize,
}
impl<R: Read> Compact<R> {
    fn bytes(&mut self, n: usize) -> Result<(), String> {
        if n > self.left {
            return Err("Thrift length exceeds the remaining header".into());
        }
        let got = std::io::copy(
            &mut self.reader.by_ref().take(n as u64),
            &mut std::io::sink(),
        )
        .map_err(|e| e.to_string())?;
        if got != n as u64 {
            return Err("Truncated Thrift header".into());
        }
        self.left -= n;
        self.used += n;
        Ok(())
    }
    fn byte(&mut self) -> Result<u8, String> {
        if self.left == 0 {
            return Err("Truncated Thrift header".into());
        }
        let mut b = [0];
        self.reader.read_exact(&mut b).map_err(|e| e.to_string())?;
        self.left -= 1;
        self.used += 1;
        Ok(b[0])
    }
    fn unsigned(&mut self) -> Result<u64, String> {
        let mut v = 0;
        for shift in (0..70).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                return Err("Thrift integer overflows".into());
            }
            v |= u64::from(b & 127) << shift;
            if b & 128 == 0 {
                return Ok(v);
            }
        }
        Err("Thrift integer overflows".into())
    }
    fn int(&mut self) -> Result<i64, String> {
        let v = self.unsigned()?;
        Ok((v >> 1) as i64 ^ -((v & 1) as i64))
    }
    fn field(&mut self, last: &mut i64) -> Result<Option<(i64, u8)>, String> {
        let b = self.byte()?;
        if b == 0 {
            return Ok(None);
        }
        let delta = i64::from(b >> 4);
        *last = if delta == 0 {
            self.int()?
        } else {
            last.checked_add(delta).ok_or("Thrift field id overflows")?
        };
        Ok(Some((*last, b & 15)))
    }
    fn list(&mut self) -> Result<(usize, u8), String> {
        let b = self.byte()?;
        let n = if b >> 4 == 15 {
            self.unsigned()?
        } else {
            u64::from(b >> 4)
        };
        if n > self.left as u64 || n > 1_000_000 {
            return Err("Thrift collection exceeds the remaining header or item limit".into());
        }
        Ok((n as usize, b & 15))
    }
    fn skip(&mut self, kind: u8) -> Result<(), String> {
        enum Task {
            Value(u8, usize),
            Struct(i64, usize),
            Items(usize, u8, usize),
            Map(usize, u8, u8, usize),
        }
        let mut tasks = vec![Task::Value(kind, 0)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Struct(mut last, depth) => {
                    if let Some((_, kind)) = self.field(&mut last)? {
                        tasks.push(Task::Struct(last, depth));
                        tasks.push(Task::Value(kind, depth));
                    }
                }
                Task::Items(n, kind, depth) if n > 0 => {
                    tasks.push(Task::Items(n - 1, kind, depth));
                    // Boolean values in collections are bytes, unlike booleans in field headers.
                    tasks.push(Task::Value(
                        if matches!(kind, 1 | 2) { 3 } else { kind },
                        depth,
                    ));
                }
                Task::Map(n, k, v, depth) if n > 0 => {
                    tasks.push(Task::Map(n - 1, k, v, depth));
                    tasks.push(Task::Value(if matches!(v, 1 | 2) { 3 } else { v }, depth));
                    tasks.push(Task::Value(if matches!(k, 1 | 2) { 3 } else { k }, depth));
                }
                Task::Value(kind, depth) => {
                    if depth > 64 {
                        return Err("Thrift nesting exceeds 64 levels".into());
                    }
                    match kind {
                        1 | 2 => {}
                        3 => {
                            self.byte()?;
                        }
                        4..=6 => {
                            self.unsigned()?;
                        }
                        7 => self.bytes(8)?,
                        8 => {
                            let n = usize::try_from(self.unsigned()?)
                                .map_err(|_| "Thrift length overflows")?;
                            self.bytes(n)?;
                        }
                        9 | 10 => {
                            let (n, k) = self.list()?;
                            tasks.push(Task::Items(n, k, depth + 1));
                        }
                        11 => {
                            let n = self.unsigned()?;
                            if n > self.left as u64 / 2 {
                                return Err("Thrift map exceeds the remaining header".into());
                            }
                            if n > 0 {
                                let kinds = self.byte()?;
                                tasks.push(Task::Map(
                                    n as usize,
                                    kinds >> 4,
                                    kinds & 15,
                                    depth + 1,
                                ));
                            }
                        }
                        12 => tasks.push(Task::Struct(0, depth + 1)),
                        _ => return Err("Invalid Thrift value type".into()),
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub(in crate::table) fn footer(bytes: &[u8], limits: Limits) -> Result<(), String> {
    let mut r = Compact {
        reader: bytes,
        left: bytes.len(),
        used: 0,
    };
    let mut last = 0;
    let mut has_schema = false;
    while let Some((id, kind)) = r.field(&mut last)? {
        if id == 4 {
            if kind != 9 {
                return Err("Expected a list of Parquet row groups".into());
            }
            let (count, kind) = r.list()?;
            if kind != 12 || count > limits.batches {
                return Err(format!(
                    "Invalid row groups or more than {} row groups",
                    limits.batches
                ));
            }
            for _ in 0..count {
                r.skip(kind)?;
            }
            continue;
        }
        if id != 2 {
            r.skip(kind)?;
            continue;
        }
        if kind != 9 {
            return Err("Expected a list of Parquet schema fields".into());
        }
        let (n, element) = r.list()?;
        if element != 12 || n == 0 || n > limits.columns.saturating_mul(2).saturating_add(1) {
            return Err("Parquet schema exceeds the column limit or has no root".into());
        }
        let mut children = vec![1usize];
        let mut leaves = 0;
        for _ in 0..n {
            while children.last() == Some(&0) {
                children.pop();
            }
            let left = children
                .last_mut()
                .ok_or("Extra field outside the Parquet schema root")?;
            *left -= 1;
            let mut fields = 0;
            let mut count = 0;
            while let Some((id, kind)) = r.field(&mut fields)? {
                if id == 5 {
                    if kind != 5 {
                        return Err("Invalid Parquet child count".into());
                    }
                    count =
                        usize::try_from(r.int()?).map_err(|_| "Negative Parquet child count")?;
                    if count > n {
                        return Err("Parquet child count exceeds the schema".into());
                    }
                } else {
                    r.skip(kind)?;
                }
            }
            if count > 0 {
                children.push(count);
                if children.len() > 65 {
                    return Err("Parquet schema nesting exceeds 64 levels".into());
                }
            } else {
                leaves += 1;
            }
        }
        if children.iter().any(|&n| n != 0) || leaves > limits.columns {
            return Err("Incomplete Parquet schema or too many columns".into());
        }
        has_schema = true;
    }
    if !has_schema {
        return Err("Parquet footer has no schema".into());
    }
    Ok(())
}

/// Returns header bytes, encoded page bytes and decoded page bytes without reading the body.
pub(in crate::table) fn page(
    reader: impl Read,
    available: usize,
    budget: usize,
) -> Result<(usize, usize, usize), String> {
    let mut r = Compact {
        reader,
        left: available.min(16 << 20),
        used: 0,
    };
    let mut last = 0;
    let mut encoded = None;
    let mut decoded = None;
    while let Some((id, kind)) = r.field(&mut last)? {
        if matches!(id, 2 | 3) {
            if kind != 5 {
                return Err("Invalid Parquet page length type".into());
            }
            let size = usize::try_from(r.int()?).map_err(|_| "Negative Parquet page length")?;
            if id == 2 {
                decoded = Some(size);
            } else {
                encoded = Some(size);
            }
        } else if matches!(id, 5 | 7 | 8) && kind == 12 {
            let mut nested = 0;
            while let Some((id, kind)) = r.field(&mut nested)? {
                if (1..=3).contains(&id) && kind == 5 {
                    let value = r.int()?;
                    if value < 0 || value as u64 > budget as u64 / 8 {
                        return Err(
                            "Parquet page value count exceeds the decoded byte budget".into()
                        );
                    }
                } else {
                    r.skip(kind)?;
                }
            }
        } else {
            r.skip(kind)?;
        }
    }
    let encoded = encoded.ok_or("Parquet page has no compressed length")?;
    let decoded = decoded.ok_or("Parquet page has no uncompressed length")?;
    if encoded > available.saturating_sub(r.used) {
        return Err("Parquet page extends beyond its column chunk".into());
    }
    if decoded > budget {
        return Err("Parquet page exceeds the decoded byte budget".into());
    }
    Ok((r.used, encoded, decoded))
}
